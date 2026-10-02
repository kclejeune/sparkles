//! The SPARQL 1.1 Service Description of `GET /{ds}/sparql` without a query.

use super::*;

const SD: &str = "http://www.w3.org/ns/sparql-service-description#";

async fn get(app: &Router, uri: &str, accept: Option<&str>) -> Resp {
    let mut req = Request::get(uri);
    if let Some(a) = accept {
        req = req.header(header::ACCEPT, a);
    }
    send(app, req.body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn sparql_endpoints_describe_themselves() {
    let s = server();
    let r = get(&s.app, "/ds/sparql", Some("application/n-triples")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.content_type, "application/n-triples");
    let nt = r.text();
    let has = |s: &str, p: &str, o: &str| {
        let t = format!("{s} <{SD}{p}> {o} .");
        assert!(nt.lines().any(|l| l == t), "{t} missing from\n{nt}");
    };
    let query = "<http://localhost/ds/sparql>";
    let update = "<http://localhost/ds/update>";
    has(query, "endpoint", query);
    for l in ["SPARQL10Query", "SPARQL11Query", "SPARQLQuery"] {
        has(query, "supportedLanguage", &format!("<{SD}{l}>"));
    }
    has(update, "endpoint", update);
    has(
        update,
        "supportedLanguage",
        &format!("<{SD}SPARQL11Update>"),
    );
    has(
        query,
        "resultFormat",
        "<http://www.w3.org/ns/formats/SPARQL_Results_JSON>",
    );
    has(
        update,
        "inputFormat",
        "<http://www.w3.org/ns/formats/Turtle>",
    );
    has(
        query,
        "extensionAggregate",
        "<http://jena.apache.org/ARQ/function/aggregate#median>",
    );
    has(
        query,
        "extensionAggregate",
        "<http://jena.apache.org/ARQ/function#stdev>",
    );
    has(
        query,
        "extensionFunction",
        "<http://www.w3.org/2005/xpath-functions#round-half-to-even>",
    );
    has(
        query,
        "extensionFunction",
        "<http://jena.apache.org/ARQ/function#localname>",
    );
    has(
        query,
        "propertyFeature",
        "<http://jena.apache.org/text#query>",
    );
    has(
        query,
        "defaultEntailmentRegime",
        "<http://www.w3.org/ns/entailment/Simple>",
    );
    // SERVICE is allowed (the test server's default); the default graph is not the union
    has(query, "feature", &format!("<{SD}BasicFederatedQuery>"));
    assert!(!nt.contains("UnionDefaultGraph") && !nt.contains("EmptyGraphs"));
    // the default dataset: counts, the named graph, the VoID description
    assert!(
        nt.contains(&format!("<{SD}name> <http://example.org/g1> .")),
        "{nt}"
    );
    assert!(
        nt.contains("<http://rdfs.org/ns/void#triples> \"9\"^^"),
        "{nt}"
    );
    assert!(
        nt.contains("<http://rdfs.org/ns/void#triples> \"2\"^^"),
        "{nt}"
    );
    assert!(
        nt.contains(
            "<http://www.w3.org/2000/01/rdf-schema#seeAlso> <http://localhost/$/schema/ds?format=turtle>"
        ),
        "{nt}"
    );

    // Turtle without an Accept header, at /query too, and as HEAD
    let r = get(&s.app, "/ds/query", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.content_type.starts_with("text/turtle"),
        "{}",
        r.content_type
    );
    assert!(r.text().contains("sd:Service"), "{}", r.text());
    let r = send(
        &s.app,
        Request::head("/ds/sparql").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = get(&s.app, "/ds/sparql?format=jsonld", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.content_type.starts_with("application/ld+json"));
    // a client that wants results, not RDF, still hears that the query is missing
    let r = get(
        &s.app,
        "/ds/sparql",
        Some("application/sparql-results+json"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("missing 'query'"), "{}", r.text());
    // the dataset itself stays a Graph Store read
    let r = get(&s.app, "/ds", Some("application/n-quads")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text().contains(SD), "{}", r.text());
    // the base URL comes from Host and X-Forwarded-Proto
    let r = send(
        &s.app,
        Request::get("/ds/sparql")
            .header(header::HOST, "127.0.0.1:3030")
            .header("x-forwarded-proto", "https")
            .header(header::ACCEPT, "application/n-triples")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        r.text().contains("<https://127.0.0.1:3030/ds/sparql>"),
        "{}",
        r.text()
    );
    // an unknown dataset
    let r = get(&s.app, "/nope/sparql", Some("text/turtle")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
