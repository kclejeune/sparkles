//! The dataset's query defaults over HTTP: RDFS on read and the materialized inferences
//! reach `/{ds}/sparql` and a stored query's run as they reach the library's queries,
//! and `reasoning=false` leaves the inferences out.

use super::*;
use sparkles::reasoning::rdfs::NewSchema;
use sparkles::reasoning::{INFERRED_GRAPH, ReasoningRecord};

const ASK: &str = "ASK { <http://example.org/alice> a <http://example.org/Person> }";

async fn ask(app: &Router, path: &str) -> bool {
    let r = send(app, Request::get(path).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
    r.json()["boolean"].as_bool().unwrap()
}

/// The answer over HTTP (a query, and a stored query's run) and in the library.
async fn answers(s: &Server, extra: &str) -> (bool, bool, bool) {
    let q = percent_encoding::utf8_percent_encode(ASK, percent_encoding::NON_ALPHANUMERIC);
    let http = ask(&s.app, &format!("/ds/sparql?query={q}{extra}")).await;
    let stored = ask(&s.app, &format!("/ds/queries/alice?{extra}")).await;
    let lib = s.state.get("ds").unwrap().dataset.ask(ASK).unwrap();
    (http, stored, lib)
}

fn store_query(s: &Server) {
    let def = serde_json::from_value(serde_json::json!({ "query": ASK })).unwrap();
    s.state
        .get("ds")
        .unwrap()
        .dataset
        .queries()
        .put("alice", def, Default::default())
        .unwrap();
}

#[tokio::test]
async fn configured_rdfs_reaches_http_and_stored_queries() {
    // DATA has ex:Student rdfs:subClassOf ex:Person and ex:alice a ex:Student
    let s = server();
    store_query(&s);
    assert_eq!(answers(&s, "").await, (false, false, false));
    let ds = s.state.get("ds").unwrap();
    ds.dataset
        .reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    assert_eq!(answers(&s, "").await, (true, true, true));
    ds.dataset.reasoning().rdfs().reset().unwrap();
    assert_eq!(answers(&s, "").await, (false, false, false));
}

#[tokio::test]
async fn materialized_inferences_reach_http_and_stored_queries() {
    let s = server();
    store_query(&s);
    let ds = s.state.get("ds").unwrap();
    ds.dataset
        .load_str_into(
            "<http://example.org/alice> a <http://example.org/Person> .",
            sparkles::io::RdfFormat::Turtle,
            INFERRED_GRAPH,
        )
        .unwrap();
    assert_eq!(answers(&s, "").await, (false, false, false));
    ds.dataset
        .state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(answers(&s, "").await, (true, true, true));
    // `reasoning=false` leaves them out of the query and of the stored query's run
    let (http, stored, _) = answers(&s, "&reasoning=false").await;
    assert_eq!((http, stored), (false, false));
}
