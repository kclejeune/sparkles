//! The facets route (spec U11 §3.2) against SPARQL counts over the same data: range bins
//! and missing values (A3), cross-filtering (A4), the brush text that **Apply to query**
//! writes (A5), each kind of set, handed-back edges, candidates, the request checks and
//! the budgets. Access control is tested with restricted principals in
//! `router_tests::auth::overview`.

use crate::http::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value as J, json};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const EX: &str = "http://ex/";
const COUNTRIES: [&str; 4] = ["FR", "DE", "IT", "ES"];

/// 200 people. Every tenth has no age, and person 3 has two. Each has a country, a city
/// IRI and a date joined. There are also 5 organizations with a founding year.
fn data() -> String {
    let mut t = String::from(
        "@prefix ex: <http://ex/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for c in 0..3 {
        writeln!(t, "ex:city{c} rdfs:label \"City {c}\" .").unwrap();
    }
    for i in 0..200 {
        write!(
            t,
            "ex:p{i} a ex:Person ; rdfs:label \"Person {i}\" ; ex:country \"{}\" ; ex:city ex:city{} ; ex:joined \"{}-03-15\"^^xsd:date",
            COUNTRIES[i % 4],
            i % 3,
            2000 + i % 26
        )
        .unwrap();
        if i % 10 != 0 {
            write!(t, " ; ex:age {}", 18 + (i * 37) % 73).unwrap();
        }
        t.push_str(" .\n");
    }
    t.push_str("ex:p3 ex:age 95 .\n");
    for o in 0..5 {
        writeln!(t, "ex:o{o} a ex:Org ; ex:founded {} .", 1990 + o).unwrap();
    }
    t
}

struct Server {
    _dir: tempfile::TempDir,
    app: Router,
}

fn server_with(configure: impl FnOnce(&mut AppState)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        None,
    )
    .unwrap();
    configure(&mut state);
    let state = Arc::new(state);
    let ds = state.attach("d", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data().into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    state.set_phase(Phase::Ready);
    let app = router(state.clone());
    Server { _dir: dir, app }
}

fn server() -> Server {
    server_with(|_| {})
}

async fn post(app: &Router, uri: &str, body: &J) -> (StatusCode, J) {
    let res = app
        .clone()
        .oneshot(
            Request::post(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let j = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| J::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, j)
}

async fn facets(app: &Router, body: J) -> J {
    let (s, j) = post(app, "/$/graph/d/facets", &body).await;
    assert_eq!(s, StatusCode::OK, "{body}: {j}");
    j
}

/// The rows of a SELECT, as maps from variable to value.
async fn sparql(app: &Router, q: &str) -> Vec<BTreeMap<String, String>> {
    let uri = format!(
        "/d/sparql?query={}",
        form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
    );
    let res = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header("accept", "application/sparql-results+json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let j: J = serde_json::from_slice(&bytes).unwrap();
    j["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            b.as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v["value"].as_str().unwrap().to_string()))
                .collect()
        })
        .collect()
}

async fn sparql_count(app: &Router, q: &str) -> u64 {
    let rows = sparql(app, q).await;
    rows[0]["n"].parse().unwrap()
}

fn person() -> J {
    json!({ "class": format!("{EX}Person") })
}

fn facet<'a>(answer: &'a J, p: &str) -> &'a J {
    answer["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["property"] == format!("{EX}{p}"))
        .unwrap_or_else(|| panic!("no facet {p}: {answer}"))
}

fn values(f: &J) -> BTreeMap<String, u64> {
    f["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["value"]["value"].as_str().unwrap().to_string(),
                v["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn bins_and_missing_match_sparql_counts() {
    let s = server();
    let a = facets(
        &s.app,
        json!({ "within": person(), "facets": [{ "property": format!("{EX}age") }] }),
    )
    .await;
    assert_eq!(a["size"], 200);
    assert_eq!(a["matching"], 200);
    let age = facet(&a, "age");
    assert_eq!(age["kind"], "numeric");
    let bins = age["bins"].as_array().unwrap();
    assert!(bins.len() <= 20 && bins.len() >= 10, "{age}");
    // every bin against a SPARQL count of the same range, with distinct people
    for b in bins {
        let (from, to) = (b["from"].as_f64().unwrap(), b["to"].as_f64().unwrap());
        assert_eq!(from.fract(), 0.0, "integer ages give whole edges: {b}");
        let n = sparql_count(
            &s.app,
            &format!(
                "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{EX}Person> ; <{EX}age> ?a FILTER(?a >= {from} && ?a < {to}) }}"
            ),
        )
        .await;
        assert_eq!(b["count"].as_u64().unwrap(), n, "{b}");
    }
    // A3: the bins sum to the people with an age (person 3's two ages fall in two bins)
    let total: u64 = bins.iter().map(|b| b["count"].as_u64().unwrap()).sum();
    assert_eq!(total, 180 + 1);
    assert_eq!(age["missing"], 20);
    // consecutive bins share their edges, and the widths are round
    for w in bins.windows(2) {
        assert_eq!(w[0]["to"], w[1]["from"]);
    }
}

#[tokio::test]
async fn brushes_filter_the_other_facets_and_not_their_own() {
    let s = server();
    let body = |filters: J| {
        json!({
            "within": person(),
            "facets": [{ "property": format!("{EX}age") }, { "property": format!("{EX}country") }],
            "filters": filters,
        })
    };
    let open = facets(&s.app, body(json!([]))).await;
    let brushed = facets(
        &s.app,
        body(json!([{ "property": format!("{EX}age"), "range": [30, 45] }])),
    )
    .await;
    // A4: the age facet keeps its own counts, the country facet counts the brushed people
    assert_eq!(facet(&open, "age"), facet(&brushed, "age"));
    let rows = sparql(
        &s.app,
        &format!(
            "SELECT ?c (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{EX}Person> ; <{EX}country> ?c FILTER EXISTS {{ ?s <{EX}age> ?a FILTER(?a >= 30 && ?a < 45) }} }} GROUP BY ?c"
        ),
    )
    .await;
    let expected: BTreeMap<String, u64> = rows
        .iter()
        .map(|r| (r["c"].clone(), r["n"].parse().unwrap()))
        .collect();
    let country = facet(&brushed, "country");
    assert_eq!(country["kind"], "values");
    assert_eq!(values(country), expected);
    assert_eq!(country["missing"], 0);
    assert_eq!(country["other"], 0);
    // A5: the brush text that Apply to query writes counts what `matching` says
    let applied = sparql_count(
        &s.app,
        &format!(
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{EX}Person> . ?s <{EX}age> ?age . FILTER(?age >= 30 && ?age < 45) }}"
        ),
    )
    .await;
    assert_eq!(brushed["matching"].as_u64().unwrap(), applied);
    assert!(applied > 0 && applied < 200);

    // a values brush narrows the age facet the same way
    let by_country = facets(
        &s.app,
        body(json!([{ "property": format!("{EX}country"), "values": [
            { "type": "literal", "value": "FR" }, { "type": "literal", "value": "DE" }
        ] }])),
    )
    .await;
    assert_eq!(by_country["matching"], 100);
    let ages: u64 = facet(&by_country, "age")["bins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["count"].as_u64().unwrap())
        .sum();
    let expected = sparql_count(
        &s.app,
        &format!(
            "SELECT (COUNT(*) AS ?n) WHERE {{ ?s a <{EX}Person> ; <{EX}country> ?c ; <{EX}age> ?a FILTER(?c IN (\"FR\", \"DE\")) }}"
        ),
    )
    .await;
    assert_eq!(ages, expected);
    // both brushes: matching is their intersection
    let both = facets(
        &s.app,
        body(json!([
            { "property": format!("{EX}age"), "range": [30, 45] },
            { "property": format!("{EX}country"), "values": [{ "type": "literal", "value": "FR" }] }
        ])),
    )
    .await;
    let n = sparql_count(
        &s.app,
        &format!(
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{EX}Person> . ?s <{EX}age> ?age . FILTER(?age >= 30 && ?age < 45) ?s <{EX}country> ?c . FILTER(?c IN (\"FR\")) }}"
        ),
    )
    .await;
    assert_eq!(both["matching"].as_u64().unwrap(), n);
}

#[tokio::test]
async fn dates_bin_by_year_and_values_by_frequency() {
    let s = server();
    let a = facets(
        &s.app,
        json!({ "within": person(), "facets": [
            { "property": format!("{EX}joined") },
            { "property": format!("{EX}city"), "top": 2 }
        ] }),
    )
    .await;
    let joined = facet(&a, "joined");
    assert_eq!(joined["kind"], "date");
    let bins = joined["bins"].as_array().unwrap();
    assert_eq!(bins[0]["from"], 2000.0);
    assert!(bins.last().unwrap()["to"].as_f64().unwrap() > 2025.0);
    let total: u64 = bins.iter().map(|b| b["count"].as_u64().unwrap()).sum();
    assert_eq!(total, 200);
    // a date brush is in whole years
    let b = facets(
        &s.app,
        json!({ "within": person(), "facets": [{ "property": format!("{EX}country") }],
                "filters": [{ "property": format!("{EX}joined"), "kind": "date", "range": [2000, 2005] }] }),
    )
    .await;
    let n = sparql_count(
        &s.app,
        &format!(
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{EX}Person> ; <{EX}joined> ?d FILTER(YEAR(?d) >= 2000 && YEAR(?d) < 2005) }}"
        ),
    )
    .await;
    assert_eq!(b["matching"].as_u64().unwrap(), n);

    let city = facet(&a, "city");
    let vals = city["values"].as_array().unwrap();
    assert_eq!(vals.len(), 2);
    assert_eq!(vals[0]["value"]["type"], "uri");
    assert_eq!(vals[0]["label"], "City 0");
    assert_eq!(vals[0]["count"], 67);
    // the third city is "other"
    assert_eq!(city["other"], 66);
    assert_eq!(city["missing"], 0);
}

#[tokio::test]
async fn edges_and_bars_handed_back_give_the_same_counts() {
    let s = server();
    let first = facets(
        &s.app,
        json!({ "within": person(), "facets": [
            { "property": format!("{EX}age") }, { "property": format!("{EX}country") }
        ] }),
    )
    .await;
    let edges: Vec<J> = {
        let bins = facet(&first, "age")["bins"].as_array().unwrap();
        let mut e: Vec<J> = bins.iter().map(|b| b["from"].clone()).collect();
        e.push(bins.last().unwrap()["to"].clone());
        e
    };
    let bars: Vec<J> = facet(&first, "country")["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["value"].clone())
        .collect();
    let again = facets(
        &s.app,
        json!({ "within": person(), "facets": [
            { "property": format!("{EX}age"), "kind": "numeric", "edges": edges },
            { "property": format!("{EX}country"), "kind": "values", "bars": bars }
        ] }),
    )
    .await;
    assert_eq!(facet(&first, "age"), facet(&again, "age"));
    assert_eq!(facet(&first, "country"), facet(&again, "country"));
}

#[tokio::test]
async fn node_and_query_sets() {
    let s = server();
    let nodes: Vec<String> = (0..20).map(|i| format!("{EX}p{i}")).collect();
    let a = facets(
        &s.app,
        json!({ "within": { "nodes": nodes }, "facets": [{ "property": format!("{EX}country") }] }),
    )
    .await;
    assert_eq!(a["size"], 20);
    assert_eq!(values(facet(&a, "country"))["FR"], 5);

    // a query set: its prefixes, a projected ?s of its own and a limit
    let q = "PREFIX ex: <http://ex/>\nSELECT ?s ?who WHERE { ?who a ex:Person ; ex:country \"IT\" . BIND(1 AS ?s) } ORDER BY ?who LIMIT 30";
    let b = facets(
        &s.app,
        json!({ "within": { "query": q, "variable": "who" }, "facets": [{ "property": format!("{EX}country") }],
                "filters": [{ "property": format!("{EX}age"), "range": [18, 60] }] }),
    )
    .await;
    assert_eq!(b["size"], 30);
    assert_eq!(
        values(facet(&b, "country"))["IT"],
        b["matching"].as_u64().unwrap()
    );

    for (body, why) in [
        (
            json!({ "within": { "query": "ASK { ?s ?p ?o }", "variable": "s" }, "candidates": true }),
            "SELECT",
        ),
        (
            json!({ "within": { "query": "SELECT ?a WHERE { ?a ?p ?o }", "variable": "s" }, "candidates": true }),
            "project",
        ),
        (
            json!({ "within": { "query": "SELECT ?s FROM <http://g> WHERE { ?s ?p ?o }", "variable": "s" }, "candidates": true }),
            "FROM",
        ),
        (
            json!({ "within": { "query": "SELECT ?s WHERE { ?s ?p }", "variable": "s" }, "candidates": true }),
            "syntax",
        ),
    ] {
        let (st, j) = post(&s.app, "/$/graph/d/facets", &body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}: {j}");
        assert!(j.to_string().contains(why), "{why}: {j}");
    }
}

#[tokio::test]
async fn candidates_rank_the_properties_of_the_set() {
    let s = server();
    let a = facets(&s.app, json!({ "within": person(), "candidates": true })).await;
    let c: BTreeMap<String, (f64, String)> = a["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["property"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches(EX)
                    .to_string(),
                (
                    c["share"].as_f64().unwrap(),
                    c["kind"].as_str().unwrap().to_string(),
                ),
            )
        })
        .collect();
    assert_eq!(c["age"], (0.9, "numeric".to_string()));
    assert_eq!(c["country"], (1.0, "values".to_string()));
    assert_eq!(c["joined"].1, "date");
    assert_eq!(c["city"].1, "values");
    // the labels and the class are not offered
    assert!(
        !c.keys().any(|k| k.contains("label") || k.contains("type")),
        "{c:?}"
    );
    assert_eq!(a["truncated"], false);
    assert_eq!(a["facets"], json!([]));
}

#[tokio::test]
async fn requests_are_checked_and_budgets_apply() {
    let s = server();
    for body in [
        json!({ "within": person() }),
        json!({ "within": person(), "facets": [{ "property": "not an iri" }] }),
        json!({ "within": person(), "facets": [{ "property": format!("{EX}age"), "bins": 1 }] }),
        json!({ "within": person(), "candidates": true, "filters": [{ "property": format!("{EX}age"), "range": [5, 1] }] }),
        json!({ "within": person(), "candidates": true, "filters": [{ "property": format!("{EX}joined"), "kind": "date", "range": [2000.5, 2003] }] }),
        json!({ "within": person(), "candidates": true, "branch": "dev" }),
        json!({ "within": { "nodes": [] }, "candidates": true }),
    ] {
        let (st, j) = post(&s.app, "/$/graph/d/facets", &body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}: {j}");
    }
    // a facet over the memory budget says why, and the answer still counts the set
    let s = server_with(|st| st.limits.query_memory_bytes = Some(2048));
    let j = facets(
        &s.app,
        json!({ "within": person(), "facets": [{ "property": format!("{EX}age") }] }),
    )
    .await;
    assert_eq!(j["size"], 200, "{j}");
    assert_eq!(facet(&j, "age")["error"]["budget"], "memory", "{j}");
    // a budget too small to count the set answers 507, like a query
    let s = server_with(|st| st.limits.query_memory_bytes = Some(64));
    let (st, j) = post(
        &s.app,
        "/$/graph/d/facets",
        &json!({ "within": person(), "facets": [{ "property": format!("{EX}age") }] }),
    )
    .await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    assert_eq!(j["budget"], "memory", "{j}");
    // a deadline that has passed answers 408
    let s = server();
    let (st, j) = post(
        &s.app,
        "/$/graph/d/facets?timeout=0.000001",
        &json!({ "within": person(), "facets": [{ "property": format!("{EX}age") }] }),
    )
    .await;
    assert_eq!(st, StatusCode::REQUEST_TIMEOUT, "{j}");
}

#[tokio::test]
async fn an_answer_reads_its_commit() {
    let s = server();
    let body = json!({ "within": { "class": format!("{EX}Org") }, "facets": [{ "property": format!("{EX}founded") }] });
    let a = facets(&s.app, body.clone()).await;
    assert_eq!(a["size"], 5);
    let res = s
        .app
        .clone()
        .oneshot(
            Request::post("/d/update")
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "PREFIX ex: <http://ex/> INSERT DATA { ex:o9 a ex:Org ; ex:founded 2020 }",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success());
    let b = facets(&s.app, body).await;
    assert_eq!(b["size"], 6);
    assert!(b["commit"].as_u64() > a["commit"].as_u64());
}
