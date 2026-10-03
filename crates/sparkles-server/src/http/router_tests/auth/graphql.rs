//! GraphQL under access control (C03 §10.2): every fetch group runs with the caller's
//! graph view and protections of triples, and the `graphql` endpoint of grants, which a
//! `query` grant covers.

use super::*;
use serde_json::json;

/// The dataset `gq`: three members of `ex:T` in three named graphs.
const DATA: &str = r#"@prefix ex: <http://ex/> .
<http://ex/c/1> { ex:d a ex:T ; ex:p "default" . }
<http://ex/a/1> { ex:a1 a ex:T ; ex:p "alpha" ; ex:q ex:b1, ex:d . }
<http://ex/b/1> { ex:b1 a ex:T ; ex:p "secret" ; ex:q ex:a1 . }
"#;

const SDL: &str = r#"extend schema @rdf(vocab: "http://ex/") @prefix(name: "ex", iri: "http://ex/")
type T { p: String q: [T!]! qOf: [T!]! @rdf(iri: "ex:q", inverse: true) }
"#;

const QUERY: &str = "{ allT(filter: { or: [{ p: { startsWith: \"a\" } }, { q: {} }, { p: { eq: \"default\" } }] }) { totalCount nodes { id p q { id p } qOf { id } } } }";

fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "qfull"
password = "{qfull}"
datasets = {{ gq = "read", hr = "read" }}

[[users]]
name = "qa"
password = "{qa}"
[[users.grants]]
dataset = "gq"
level = "read"
graphs = ["http://ex/a/*"]

[[users]]
name = "qgql"
password = "{qgql}"
[[users.grants]]
dataset = "gq"
level = "read"
endpoints = ["graphql"]

[[users]]
name = "qquery"
password = "{qquery}"
[[users.grants]]
dataset = "gq"
level = "read"
endpoints = ["query"]
"#,
        qfull = h("qfull-pw"),
        qa = h("qa-pw"),
        qgql = h("qgql-pw"),
        qquery = h("qquery-pw"),
    )
}

fn install(ds: &crate::state::Dataset, sdl: &str, data_graph: &str) {
    let mut cfg = sparkles_graphql::Config::new(sdl);
    cfg.data_graph = sparkles::guard::config::DataGraphSel::Named(data_graph.into());
    ds.graphql
        .put(cfg, Default::default(), &|_, _, _| true)
        .unwrap_or_else(|e| panic!("{}", e.message()));
}

fn server() -> AuthServer {
    let s = build(Fixture {
        extra: format!("{}\n{}", users(), super::triples::users()),
        ..Default::default()
    });
    let ds = s.state.attach("gq", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    install(&ds, SDL, "union");
    let hr = s.state.attach("hr", DbType::Mem, None).unwrap();
    hr.store
        .load(&[Source::from_bytes(
            super::triples::DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    install(
        &hr,
        r#"extend schema @rdf(vocab: "http://ex/") @prefix(name: "ex", iri: "http://ex/")
type Doc { title: String owner: String }
type Patient { name: String }
"#,
        "default",
    );
    s
}

async fn gql(app: &Router, ds: &str, user: &str, q: &str) -> R {
    call(
        app,
        "POST",
        &format!("/{ds}/graphql"),
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &json!({ "query": q }).to_string(),
    )
    .await
}

/// The data of a response, without its extensions.
fn data(r: &R) -> J {
    let j = r.json();
    assert!(j.get("errors").is_none(), "{j:#}");
    j["data"].clone()
}

#[tokio::test]
async fn groups_run_with_the_callers_graph_view() {
    let s = server();
    let full = data(&gql(&s.app, "gq", "qfull", QUERY).await);
    assert_eq!(full["allT"]["totalCount"], 3, "{full:#}");
    let a = data(&gql(&s.app, "gq", "qa", QUERY).await);
    // the same answer as over a store that holds only the graphs of the view
    let only = sparkles::store::Store::in_memory(Default::default());
    only.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> .\n<http://ex/a/1> { ex:a1 a ex:T ; ex:p \"alpha\" ; ex:q ex:b1, ex:d . }"
            .to_vec(),
        oxrdfio::RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let mut cfg = sparkles_graphql::Config::new(SDL);
    cfg.data_graph = sparkles::guard::config::DataGraphSel::Named("union".into());
    let (c, _) = sparkles_graphql::Compiled::new(cfg, 1, &|_, _, _| true).unwrap();
    let want = sparkles_graphql::execute_on(
        &c,
        only.snapshot(),
        &sparkles_graphql::Request {
            query: QUERY.into(),
            ..Default::default()
        },
        &Default::default(),
    );
    assert_eq!(a, want.body["data"], "{a:#}");
    assert_eq!(a["allT"]["totalCount"], 1);
    // a hidden node and a missing one look the same
    let q = r#"{ a: t(id: "http://ex/b1") { id } b: t(id: "http://ex/nobody") { id } }"#;
    assert_eq!(
        data(&gql(&s.app, "gq", "qa", q).await),
        json!({ "a": null, "b": null })
    );
    assert_eq!(
        data(&gql(&s.app, "gq", "qfull", q).await),
        json!({ "a": { "id": "http://ex/b1" }, "b": null })
    );
}

#[tokio::test]
async fn the_graphql_endpoint_of_grants() {
    let s = server();
    let q = "{ allT { totalCount } }";
    for user in ["qgql", "qquery", "qfull"] {
        let r = gql(&s.app, "gq", user, q).await;
        assert_eq!(r.status, StatusCode::OK, "{user}: {}", r.text());
        assert_eq!(data(&r)["allT"]["totalCount"], 3);
    }
    // a grant of the graphql endpoint alone does not reach SPARQL
    let r = call(
        &s.app,
        "GET",
        "/gq/sparql?query=ASK%7B%7D",
        &[("authorization", &b("qgql"))],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // the schema is readable through the endpoint too
    let r = call(
        &s.app,
        "GET",
        "/gq/graphql/schema",
        &[("authorization", &b("qgql"))],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("type T implements Node"), "{}", r.text());
    // changing the configuration needs admin
    let r = call(
        &s.app,
        "PUT",
        "/$/graphql/gq",
        &[
            ("authorization", &b("qfull")),
            ("content-type", "application/graphql"),
        ],
        SDL,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn protections_of_triples_apply() {
    let s = server();
    let q = "{ allDoc { nodes { id title } } allPatient { totalCount nodes { name } } }";
    let carol = data(&gql(&s.app, "hr", "tcarol", q).await);
    assert_eq!(
        carol,
        json!({ "allDoc": { "nodes": [{ "id": "http://ex/d1", "title": "carol's" }] },
                "allPatient": { "totalCount": 0, "nodes": [] } }),
    );
    let doc = data(&gql(&s.app, "hr", "tdoc", q).await);
    assert_eq!(
        doc["allPatient"]["nodes"],
        json!([{ "name": "Bob" }]),
        "{doc:#}"
    );
    assert_eq!(doc["allDoc"]["nodes"], json!([]));
    let admin = data(&gql(&s.app, "hr", "tadmin", q).await);
    assert_eq!(admin["allDoc"]["nodes"].as_array().unwrap().len(), 2);
}
