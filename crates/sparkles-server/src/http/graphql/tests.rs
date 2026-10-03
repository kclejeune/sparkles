//! The GraphQL endpoint over HTTP: the acceptance examples of C03 that need the server
//! (drafts, installs, pages across commits, status codes, budgets, explanation).

use crate::http::router;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value as J, json};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// The `org` fixture: 40 people with one name each, an age, zero to three emails, people
/// they know, an employer and English and French labels; employees are people; one org
/// has a population over 32 bits.
fn data() -> String {
    let mut s = String::from(
        "@prefix ex: <http://example.org/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         ex:Employee rdfs:subClassOf ex:Person .\n\
         ex:o1 a ex:Org ; ex:name \"Acme\" ; ex:population 3000000000 .\n\
         ex:o2 a ex:Org ; ex:name \"Beta\" ; ex:population 12 .\n",
    );
    for i in 1..=40 {
        let class = if i % 5 == 0 {
            "ex:Employee"
        } else {
            "ex:Person"
        };
        let name = if i == 3 {
            "Ann".to_string()
        } else {
            format!("P{i:02}")
        };
        s.push_str(&format!(
            "ex:p{i:02} a {class} ; ex:name \"{name}\" ; ex:age {} ; ex:worksFor ex:o{} ; ex:knows ex:p{:02}",
            20 + i,
            1 + i % 2,
            1 + i % 40
        ));
        if i % 7 == 0 {
            s.push_str(", ex:p03");
        }
        for e in 0..(i % 4) {
            s.push_str(&format!(" ; ex:email \"p{i}.{e}@x.org\""));
        }
        s.push_str(&format!(" ; rdfs:label \"p{i}\"@en"));
        if i % 2 == 0 {
            s.push_str(&format!(", \"p{i} fr\"@fr"));
        }
        s.push_str(" .\n");
    }
    s
}

const SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://example.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ] ;
  sh:property [ sh:path ex:age ; sh:datatype xsd:integer ; sh:maxCount 1 ] ;
  sh:property [ sh:path ex:knows ; sh:class ex:Person ] .
ex:EmployeeShape a sh:NodeShape ; sh:targetClass ex:Employee ;
  sh:property [ sh:path ex:worksFor ; sh:class ex:Org ; sh:maxCount 1 ] .
"#;

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = state.create("org", DbType::Persistent).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data().into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
    fn header(&self, h: &str) -> String {
        self.headers
            .get(h)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
    fn codes(&self) -> Vec<String> {
        self.json()["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| e["extensions"]["code"].as_str().unwrap_or("").to_string())
            .collect()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn get(app: &Router, uri: &str) -> Resp {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn put(app: &Router, uri: &str, ct: &str, body: &str) -> Resp {
    send(
        app,
        Request::put(uri)
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn gql_with(app: &Router, query: &str, params: &str, accept: &str) -> Resp {
    send(
        app,
        Request::post(format!("/org/graphql{params}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, accept)
            .body(Body::from(json!({ "query": query }).to_string()))
            .unwrap(),
    )
    .await
}

async fn gql(app: &Router, query: &str) -> Resp {
    gql_with(app, query, "", "application/json").await
}

const SDL: &str = r#"extend schema
  @rdf(vocab: "http://example.org/")
  @prefix(name: "ex", iri: "http://example.org/")
  @prefix(name: "rdfs", iri: "http://www.w3.org/2000/01/rdf-schema#")
type Person {
  name: String
  age: Int
  email: [String!]!
  knows: [Person!]!
  worksFor: Org
  label: String @rdf(iri: "rdfs:label")
}
type Employee { name: String }
type Org { name: String population: Integer }
"#;

async fn install(app: &Router, sdl: &str) -> Resp {
    put(app, "/$/graphql/org", "application/graphql", sdl).await
}

#[tokio::test]
async fn drafts_installs_and_versions() {
    let s = server();
    // no schema yet
    let r = gql(&s.app, "{ __typename }").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert!(
        r.json()["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("/$/graphql/org/draft")
    );
    // A1: from the guard's shapes, only the enforced name is non-null
    let r = put(
        &s.app,
        "/$/validation/org",
        "application/json",
        &json!({ "mode": "reject", "shapes": { "inline": SHAPES } }).to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = get(&s.app, "/$/graphql/org/draft?source=shapes").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let sdl = &r.body;
    assert!(
        sdl.contains("type Person @rdf(iri: \"ex:Person\")"),
        "{sdl}"
    );
    assert!(sdl.contains("name: String! @rdf"), "{sdl}");
    assert!(sdl.contains("age: Int @rdf"), "{sdl}");
    assert_eq!(
        sdl.lines()
            .filter(|l| l.contains("! @") && !l.contains("]! @"))
            .count(),
        1,
        "{sdl}"
    );
    assert!(sdl.contains("type Employee"), "{sdl}");
    let shapes_draft = r.body.clone();
    // A2: from the data
    let r = get(&s.app, "/$/graphql/org/draft?source=observed").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let sdl = &r.body;
    assert!(sdl.contains("name: String @rdf(iri: \"ex:name\")"), "{sdl}");
    assert!(
        sdl.contains("observed at most one value per instance"),
        "{sdl}"
    );
    assert!(sdl.contains("email: [String!]!"), "{sdl}");
    assert!(sdl.contains("knows: [Person!]!"), "{sdl}");
    assert!(sdl.contains("population: Integer"), "{sdl}");
    let j = get(&s.app, "/$/graphql/org/draft?source=observed&format=json")
        .await
        .json();
    assert_eq!(j["source"], "observed");
    assert!(j["types"].as_array().unwrap().len() >= 3);
    // A3: the shapes draft installs as version 1
    let r = install(&s.app, &shapes_draft).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.json()["version"], 1);
    assert_eq!(r.json()["warnings"], json!([]), "{}", r.body);
    // a non-null field no guard backs is a warning
    let r = install(
        &s.app,
        &shapes_draft.replace("age: Int @rdf", "age: Int! @rdf"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json()["version"], 2);
    let w = r.json()["warnings"].to_string();
    assert!(w.contains("Person.age"), "{w}");
    // the same configuration again adds no version
    let again = get(&s.app, "/$/graphql/org").await;
    let r = put(&s.app, "/$/graphql/org", "application/json", &again.body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.json()["changed"], false);
    assert_eq!(again.header("etag"), "\"v2\"");
    let v = get(&s.app, "/$/graphql/org/versions").await.json();
    assert_eq!(v["versions"].as_array().unwrap().len(), 2);
    // an invalid schema names its line
    let r = install(&s.app, "type Person { name: String }\n type X { y: Nope }").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json()["errors"][0]["line"].is_number(), "{}", r.body);
    // the file is kept in the database directory
    let root = s
        .state
        .get("org")
        .unwrap()
        .store
        .root()
        .unwrap()
        .to_path_buf();
    assert!(root.join("graphql.json").exists());
    let r = send(
        &s.app,
        Request::delete("/$/graphql/org")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(!root.join("graphql.json").exists());
}

#[tokio::test]
async fn queries_pages_and_status_codes() {
    let s = server();
    assert_eq!(install(&s.app, SDL).await.status, StatusCode::CREATED);
    // A4
    let r = gql(
        &s.app,
        r#"{ person(id: "http://example.org/p03") { id name age } }"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json()["data"],
        json!({ "person": { "id": "http://example.org/p03", "name": "Ann", "age": 23 } })
    );
    let commit = r.header("sparkles-commit");
    assert!(!commit.is_empty());
    assert_eq!(
        r.json()["extensions"]["sparkles"]["commit"].to_string(),
        commit
    );
    // A9
    let r = gql(
        &s.app,
        r#"{ allPerson(first: 2, orderBy: [AGE_ASC]) { nodes { fr: label(lang: ["fr", "en"]) de: label(lang: ["de"]) } } }"#,
    )
    .await;
    assert_eq!(
        r.json()["data"]["allPerson"]["nodes"],
        json!([{ "fr": "p1", "de": null }, { "fr": "p2 fr", "de": null }]),
        "{}",
        r.body
    );
    // A6: pages read the commit of their cursor
    let r = gql(
        &s.app,
        "{ allPerson(first: 2) { nodes { id } pageInfo { endCursor } } }",
    )
    .await;
    let page1 = r.json();
    let end = page1["data"]["allPerson"]["pageInfo"]["endCursor"]
        .as_str()
        .unwrap()
        .to_string();
    let ds = s.state.get("org").unwrap();
    sparkles::sparql::update::update(
        &ds.store,
        "INSERT DATA { <http://example.org/a00> a <http://example.org/Person> }",
        &Default::default(),
    )
    .unwrap();
    let next = format!("{{ allPerson(first: 2, after: \"{end}\") {{ nodes {{ id }} }} }}");
    let r = gql(&s.app, &next).await;
    assert_eq!(
        r.json()["data"]["allPerson"]["nodes"],
        json!([{ "id": "http://example.org/p03" }, { "id": "http://example.org/p04" }]),
        "{}",
        r.body
    );
    assert_eq!(r.header("sparkles-commit"), commit);
    // the head has the new person first
    let r = gql(&s.app, "{ allPerson(first: 1) { nodes { id } } }").await;
    assert_eq!(
        r.json()["data"]["allPerson"]["nodes"][0]["id"],
        "http://example.org/a00"
    );
    // after a compaction drops the commit, the cursor has expired
    ds.store.compact().unwrap();
    let r = gql(&s.app, &next).await;
    assert_eq!(r.codes(), ["CURSOR_EXPIRED"], "{}", r.body);
    // A11: limits are 422 under graphql-response+json and 200 under json
    let big = "{ allPerson(first: 1000) { nodes { knows(first: 1000) { name } } } }";
    let r = gql_with(&s.app, big, "", "application/graphql-response+json").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        r.header("content-type"),
        "application/graphql-response+json"
    );
    assert_eq!(r.json()["errors"][0]["extensions"]["estimate"], 1001000);
    assert!(r.json().get("data").is_none());
    let r = gql(&s.app, big).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.codes(), ["QUERY_TOO_COMPLEX"]);
    let r = gql_with(
        &s.app,
        "{ allPerson {",
        "",
        "application/graphql-response+json",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.codes(), ["GRAPHQL_PARSE_FAILED"]);
    // A15: mutations
    let r = gql_with(
        &s.app,
        "mutation { x }",
        "",
        "application/graphql-response+json",
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.codes(), ["GRAPHQL_VALIDATION_FAILED"]);
    let r = get(&s.app, "/org/graphql?query=mutation%20%7B%20x%20%7D").await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    // GET with variables
    let r = get(
        &s.app,
        "/org/graphql?query=query%20Q(%24id%3A%20ID!)%20%7B%20person(id%3A%20%24id)%20%7B%20name%20%7D%20%7D&variables=%7B%22id%22%3A%22http%3A%2F%2Fexample.org%2Fp03%22%7D",
    )
    .await;
    assert_eq!(r.json()["data"]["person"]["name"], "Ann", "{}", r.body);
    // a body that is not JSON
    let r = send(
        &s.app,
        Request::post("/org/graphql")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.codes(), ["BAD_REQUEST"]);
    // A14: the standard introspection query is deeper than maxDepth
    let r = gql(&s.app, INTROSPECTION).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json().get("errors").is_none(), "{}", r.body);
    assert!(
        r.json()["data"]["__schema"]["types"]
            .as_array()
            .unwrap()
            .len()
            > 20
    );
    // the API schema
    let r = get(&s.app, "/org/graphql/schema").await;
    assert!(r.body.contains("allPerson("), "{}", r.body);
    assert!(!r.body.contains("@rdf"), "{}", r.body);
}

#[tokio::test]
async fn budgets_explanation_and_metrics() {
    let s = server();
    assert_eq!(install(&s.app, SDL).await.status, StatusCode::CREATED);
    // A12
    let r = gql_with(
        &s.app,
        "{ allPerson(first: 1000) { nodes { name email knows(first: 10) { name } } } }",
        "?max-rows-produced=10",
        "application/json",
    )
    .await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.body);
    assert_eq!(r.codes(), ["BUDGET_EXCEEDED"]);
    let e = &r.json()["errors"][0]["extensions"];
    assert_eq!(e["budget"], "rows-produced");
    assert!(e["limit"].is_number() && e["requested"].is_number(), "{e}");
    assert!(r.json().get("data").is_none());
    // A5 and A17: two groups, and each group's SPARQL returns its rows on /org/sparql
    let r = gql_with(
        &s.app,
        "{ allPerson(first: 1000) { nodes { name knows(first: 10) { name } } } }",
        "?explain=true",
        "application/json",
    )
    .await;
    let j = r.json();
    assert!(j.get("errors").is_none(), "{j:#}");
    let plan = j["extensions"]["sparkles"]["plan"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(plan.len(), 2);
    assert_eq!(j["extensions"]["sparkles"]["groups"], 2);
    for g in plan {
        let q: String =
            form_urlencoded::byte_serialize(g["sparql"].as_str().unwrap().as_bytes()).collect();
        let r = send(
            &s.app,
            Request::get(format!("/org/sparql?query={q}"))
                .header(header::ACCEPT, "application/sparql-results+json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let n = r.json()["results"]["bindings"].as_array().unwrap().len();
        assert_eq!(n as u64, g["rows"].as_u64().unwrap(), "{g}");
    }
    let m = get(&s.app, "/$/metrics").await.body;
    assert!(
        m.contains("sparkles_graphql_groups_bucket{dataset=\"org\",le=\"2\"}"),
        "{m}"
    );
    assert!(m.contains("operation=\"graphql\""));
}

const INTROSPECTION: &str = r#"
query IntrospectionQuery {
  __schema {
    queryType { name } mutationType { name } subscriptionType { name }
    types { ...FullType }
    directives { name description locations args { ...InputValue } }
  }
}
fragment FullType on __Type {
  kind name description
  fields(includeDeprecated: true) { name description args { ...InputValue } type { ...TypeRef } isDeprecated deprecationReason }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason }
  possibleTypes { ...TypeRef }
}
fragment InputValue on __InputValue { name description type { ...TypeRef } defaultValue }
fragment TypeRef on __Type {
  kind name
  ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } }
}
"#;
