//! The groups route against SPARQL counts over the same data, with communities written
//! as F14's write-back writes them, property buckets, members, the limits, the budgets
//! and the memory pool. Access control is tested with restricted principals in
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
const COMMUNITY: &str = "http://ex/community";

/// 40 people in 4 coarse communities of 10, each split into two fine communities of 5,
/// written as F14's hierarchical write-back writes them; a city and an age each; and
/// links: everyone knows the next person of the same fine community, the first person
/// of every fine community knows the first of the next coarse community, and everyone
/// works for one of 3 organizations.
fn data() -> String {
    let mut t = String::from(
        "@prefix ex: <http://ex/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n@prefix skos: <http://www.w3.org/2004/02/skos/core#> .\n",
    );
    let cities = ["Oslo", "Kyoto", "Paris"];
    for o in 0..3 {
        writeln!(t, "ex:org{o} a ex:Org ; rdfs:label \"Org {o}\" .").unwrap();
    }
    for c in 0..4 {
        writeln!(t, "<urn:x-sparkles:algo:r1:1:{c}> ex:level 1 .").unwrap();
        for f in 0..2 {
            let fine = c * 2 + f;
            writeln!(
                t,
                "<urn:x-sparkles:algo:r1:0:{fine}> ex:level 0 ; skos:broader <urn:x-sparkles:algo:r1:1:{c}> ."
            )
            .unwrap();
        }
    }
    for i in 0..40 {
        let (c, fine) = (i / 10, i / 5);
        writeln!(
            t,
            "ex:p{i} a ex:Person ; rdfs:label \"Person {i}\" ; ex:city \"{}\" ; ex:age {} ; ex:worksFor ex:org{} ; ex:community <urn:x-sparkles:algo:r1:1:{c}>, <urn:x-sparkles:algo:r1:0:{fine}> .",
            cities[i % 3],
            20 + i,
            i % 3
        )
        .unwrap();
        if i % 5 != 4 {
            writeln!(t, "ex:p{i} ex:knows ex:p{} .", i + 1).unwrap();
        }
        if i % 5 == 0 {
            writeln!(t, "ex:p{i} ex:knows ex:p{} .", (i + 10) % 40).unwrap();
        }
    }
    t
}

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server_with(pool_mb: Option<u64>, configure: impl FnOnce(&mut AppState)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        pool_mb.map(|m| m << 20),
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
    Server {
        _dir: dir,
        state,
        app,
    }
}

fn server() -> Server {
    server_with(None, |_| {})
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

async fn groups(app: &Router, body: J) -> J {
    let (s, j) = post(app, "/$/graph/d/groups", &body).await;
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

fn sizes(answer: &J) -> BTreeMap<String, u64> {
    answer["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["label"].as_str().unwrap().to_string(),
                g["size"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn group_id(answer: &J, label: &str) -> String {
    answer["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["label"] == label)
        .unwrap_or_else(|| panic!("no group {label}: {answer}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The count of the edges from `source` to `target`, over all predicates.
fn edge_count(answer: &J, source: &str, target: &str) -> u64 {
    answer["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["source"] == source && e["target"] == target)
        .map(|e| e["count"].as_u64().unwrap())
        .sum()
}

#[tokio::test]
async fn a_small_class_opens_into_its_members_with_their_links() {
    let s = server();
    let a = groups(&s.app, json!({ "within": { "class": format!("{EX}Org") } })).await;
    assert_eq!(a["grouping"]["kind"], "members", "{a}");
    assert_eq!(a["within"]["size"], 3);
    let members = a["members"].as_array().unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0]["label"], "Org 0");
    assert_eq!(members[0]["class"], format!("{EX}Org"));
    // each organization is linked from its people, as an edge from the class Person
    for o in 0..3 {
        let rows = sparql(
            &s.app,
            &format!(
                "SELECT (COUNT(*) AS ?n) {{ ?s a <{EX}Person> ; <{EX}worksFor> <{EX}org{o}> }}"
            ),
        )
        .await;
        let n: u64 = rows[0]["n"].parse().unwrap();
        assert_eq!(
            edge_count(&a, &format!("c:{EX}Person"), &format!("m:{EX}org{o}")),
            n,
            "{a}"
        );
    }
    assert_eq!(a["truncated"], false);
    assert!(a["commit"].is_u64());
}

#[tokio::test]
async fn a_large_class_opens_into_communities_and_then_into_members() {
    let s = server();
    // with a limit of 20, the 40 people are too many to list: F14's communities group them
    let a = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Person") }, "limit": 20 }),
    )
    .await;
    assert_eq!(a["grouping"]["kind"], "communities", "{a}");
    assert_eq!(a["grouping"]["predicate"], COMMUNITY);
    assert_eq!(a["communities"], true);
    let sz = sizes(&a);
    assert_eq!(sz.len(), 4, "{a}");
    assert_eq!(sz.values().sum::<u64>(), 40);
    assert!(sz.values().all(|n| *n == 10));
    // the first members of the largest groups
    assert_eq!(a["groups"][0]["top"].as_array().unwrap().len(), 3, "{a}");
    // the links between coarse communities: the first person of each fine community
    // knows the first of the next coarse community
    let c0 = group_id(&a, "Community 0");
    let c1 = group_id(&a, "Community 1");
    assert_eq!(
        edge_count(&a, &format!("g:{c0}"), &format!("g:{c1}")),
        2,
        "{a}"
    );
    // the links inside a community are a self edge of its group
    assert_eq!(
        edge_count(&a, &format!("g:{c0}"), &format!("g:{c0}")),
        8,
        "{a}"
    );
    // and the links to the other classes
    let orgs: u64 = a["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["target"] == format!("c:{EX}Org"))
        .map(|e| e["count"].as_u64().unwrap())
        .sum();
    assert_eq!(orgs, 40);

    // a coarse community opens into its fine ones
    let b = groups(&s.app, json!({ "within": { "group": c0 }, "limit": 3 })).await;
    assert_eq!(b["grouping"]["kind"], "communities", "{b}");
    let sz = sizes(&b);
    assert_eq!(sz.len(), 2, "{b}");
    assert_eq!(sz.values().sum::<u64>(), 10);
    // with edges to the sibling coarse communities, named as the first answer named them
    let f0 = group_id(&b, "Community 0");
    assert_eq!(
        edge_count(&b, &format!("g:{f0}"), &format!("g:{c1}")),
        1,
        "{b}"
    );

    // a fine community lists its members, with edges to the sibling groups on its path
    let m = groups(&s.app, json!({ "within": { "group": f0 } })).await;
    assert_eq!(m["grouping"]["kind"], "members", "{m}");
    let members = m["members"].as_array().unwrap();
    assert_eq!(members.len(), 5);
    assert_eq!(
        edge_count(&m, &format!("m:{EX}p0"), &format!("m:{EX}p1")),
        1,
        "{m}"
    );
    assert_eq!(
        edge_count(&m, &format!("m:{EX}p0"), &format!("g:{c1}")),
        1,
        "{m}"
    );
    // p4 is known by p3 only, and p0 is known from community 3
    let c3 = group_id(&a, "Community 3");
    assert_eq!(
        edge_count(&m, &format!("g:{c3}"), &format!("m:{EX}p0")),
        1,
        "{m}"
    );
    let f1 = group_id(&b, "Community 1");
    assert_eq!(
        edge_count(&m, &format!("m:{EX}p0"), &format!("g:{f1}")),
        0,
        "{m}"
    );
    assert!(m["groups"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn property_buckets_match_sparql_group_by() {
    let s = server();
    let a = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city") } }),
    )
    .await;
    assert_eq!(a["grouping"]["kind"], "values", "{a}");
    assert_eq!(a["communities"], false);
    let rows = sparql(
        &s.app,
        &format!("SELECT ?c (COUNT(DISTINCT ?s) AS ?n) {{ ?s a <{EX}Person> ; <{EX}city> ?c }} GROUP BY ?c"),
    )
    .await;
    let want: BTreeMap<String, u64> = rows
        .iter()
        .map(|r| (r["c"].clone(), r["n"].parse().unwrap()))
        .collect();
    assert_eq!(sizes(&a), want);
    // the edges between two cities' groups, against SPARQL
    let oslo = group_id(&a, "Oslo");
    let kyoto = group_id(&a, "Kyoto");
    let rows = sparql(
        &s.app,
        &format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s a <{EX}Person> ; <{EX}city> \"Oslo\" ; ?p ?o . ?o a <{EX}Person> ; <{EX}city> \"Kyoto\" }}"
        ),
    )
    .await;
    let n: u64 = rows[0]["n"].parse().unwrap();
    assert!(n > 0);
    assert_eq!(
        edge_count(&a, &format!("g:{oslo}"), &format!("g:{kyoto}")),
        n,
        "{a}"
    );

    // two buckets: the most frequent value and the rest
    let b = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city"), "buckets": 2 } }),
    )
    .await;
    let sz = sizes(&b);
    assert_eq!(sz.len(), 2, "{b}");
    assert!(sz.contains_key("Other values"), "{b}");
    assert_eq!(sz.values().sum::<u64>(), 40);

    // a numeric property with more values than buckets is grouped by ranges
    let c = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}age"), "buckets": 4 } }),
    )
    .await;
    assert_eq!(c["grouping"]["kind"], "ranges", "{c}");
    let sz = sizes(&c);
    assert_eq!(sz.len(), 4, "{c}");
    assert_eq!(sz.values().sum::<u64>(), 40);
    assert_eq!(sz["20 – 29"], 10, "{c}");
    // a range opens into its members
    let r = groups(
        &s.app,
        json!({ "within": { "group": group_id(&c, "20 – 29") } }),
    )
    .await;
    assert_eq!(r["members"].as_array().unwrap().len(), 10, "{r}");
}

#[tokio::test]
async fn limits_and_bad_requests() {
    let s = server();
    // members past the limit are cut, and the answer says so
    let a = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Person") }, "by": "members", "limit": 7 }),
    )
    .await;
    assert_eq!(a["members"].as_array().unwrap().len(), 7);
    assert_eq!(a["truncated"], true);
    assert_eq!(a["within"]["size"], 40);
    for bad in [
        json!({}),
        json!([]),
        json!({ "within": { "class": "not an iri" } }),
        json!({ "within": { "class": "http://ex/a> } <x" } }),
        json!({ "within": { "group": "g1.nope" } }),
        json!({ "within": { "class": format!("{EX}Person") }, "limit": 5001 }),
        json!({ "within": { "class": format!("{EX}Person") }, "limit": 0 }),
        json!({ "within": { "class": format!("{EX}Person") }, "by": "everything" }),
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city"), "buckets": 1 } }),
        json!({ "within": { "class": format!("{EX}Person") }, "branch": "dev" }),
        json!({ "within": { "class": format!("{EX}Person") }, "extra": 1 }),
        json!({ "within": { "class": format!("{EX}Person") }, "at": "not a state" }),
        // no communities or values to group by
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "community": format!("{EX}city") } }),
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}nothing") } }),
    ] {
        let (st, j) = post(&s.app, "/$/graph/d/groups", &bad).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {j}");
    }
    let (st, _) = post(
        &s.app,
        "/$/graph/nope/groups",
        &json!({ "within": { "class": format!("{EX}Person") } }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // an unknown class is an empty answer
    let a = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Nothing") } }),
    )
    .await;
    assert_eq!(a["within"]["size"], 0);
    assert!(a["members"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn answers_name_their_commit_and_follow_writes() {
    let s = server();
    let body = json!({ "within": { "class": format!("{EX}Org") } });
    let a = groups(&s.app, body.clone()).await;
    let again = groups(&s.app, body.clone()).await;
    assert_eq!(a, again, "the same commit gives the same answer");
    let ds = s.state.get("d").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<http://ex/org9> a <http://ex/Org> .".to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let b = groups(&s.app, body).await;
    assert_eq!(b["members"].as_array().unwrap().len(), 4, "{b}");
    assert!(b["commit"].as_u64() > a["commit"].as_u64());
}

#[tokio::test]
async fn budgets_deadlines_and_the_memory_pool_apply() {
    // a memory budget too small for the grouping answers 507, like a query
    let s = server_with(None, |st| st.limits.query_memory_bytes = Some(2048));
    let (st, j) = post(
        &s.app,
        "/$/graph/d/groups",
        &json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city") } }),
    )
    .await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    assert_eq!(j["budget"], "memory", "{j}");

    // a deadline that has passed answers 408
    let s = server();
    let (st, j) = post(
        &s.app,
        "/$/graph/d/groups?timeout=0.000001",
        &json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city") } }),
    )
    .await;
    assert_eq!(st, StatusCode::REQUEST_TIMEOUT, "{j}");

    // the request waits for its grant of the memory pool, and answers 503 when none comes
    let s = server_with(Some(64), |st| {
        st.admission.wait = Duration::from_millis(100)
    });
    let pool = s.state.catalog.pool().unwrap().clone();
    let held = pool.try_grant(56 << 20).unwrap();
    let body = json!({ "within": { "class": format!("{EX}Org") } });
    let (st, j) = post(&s.app, "/$/graph/d/groups", &body).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{j}");
    assert_eq!(j["budget"], "server-memory", "{j}");
    drop(held);
    let (st, j) = post(&s.app, "/$/graph/d/groups", &body).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(pool.granted(), 0, "the grant went back with the request");
}

#[test]
fn a_cancelled_request_stops() {
    let s = server();
    let ds = s.state.get("d").unwrap();
    let mut opts = sparkles::sparql::QueryOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..Default::default()
    };
    opts.timeout = Some(Duration::from_secs(30));
    let run = super::groups::Run {
        snap: ds.store.snapshot(),
        opts: &opts,
    };
    let req = super::GroupsRequest::from_json(
        &json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city") } }),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.1));
    let Err(e) = run.answer(&req) else {
        panic!("a cancelled request answered");
    };
    assert_eq!(e.0, StatusCode::SERVICE_UNAVAILABLE, "{:?}", e.1);
}

/// Review findings of U06: a node with several values in one range is one member, the
/// limit applies to property groups, ranges stay within the requested number, and the
/// deepest group opens into members rather than into ids the route would refuse.
#[tokio::test]
async fn ranges_limits_and_depth_hold_at_their_edges() {
    let s = server();
    let ds = s.state.get("d").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://ex/> .
ex:a a ex:Q ; ex:n 22 ; ex:knows ex:b .
ex:b a ex:Q ; ex:n 22, 23 .
ex:c a ex:Q ; ex:n 60 .
ex:d a ex:Q ; ex:n 100 ."#
                .to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // values 20 to 100 in at most 2 ranges
    let by_n = json!({ "within": { "class": format!("{EX}Q") }, "by": { "property": format!("{EX}n"), "buckets": 2 } });
    let a = groups(&s.app, by_n).await;
    assert_eq!(a["grouping"]["kind"], "ranges", "{a}");
    let sz = sizes(&a);
    assert!(sz.len() <= 2, "{a}");
    // the range of a and b: one link from a to b, counted once
    let label = sz
        .keys()
        .find(|l| l.starts_with('0') || l.starts_with("20"))
        .unwrap()
        .clone();
    let m = groups(
        &s.app,
        json!({ "within": { "group": group_id(&a, &label) }, "by": "members" }),
    )
    .await;
    assert_eq!(
        edge_count(&m, &format!("m:{EX}a"), &format!("m:{EX}b")),
        1,
        "{m}"
    );
    assert_eq!(m["truncated"], false, "{m}");

    // the limit cuts property groups too
    let a = groups(
        &s.app,
        json!({ "within": { "class": format!("{EX}Person") }, "by": { "property": format!("{EX}city") }, "limit": 1 }),
    )
    .await;
    assert_eq!(a["groups"].as_array().unwrap().len(), 1, "{a}");
    assert_eq!(a["truncated"], true, "{a}");

    // eight levels deep, the group opens into its members
    let city = json!({ "property": format!("{EX}city") });
    let mut within = json!({ "class": format!("{EX}Person") });
    for _ in 0..8 {
        let a = groups(&s.app, json!({ "within": within, "by": city })).await;
        assert_eq!(a["grouping"]["kind"], "values", "{a}");
        within = json!({ "group": group_id(&a, "Oslo") });
    }
    let a = groups(&s.app, json!({ "within": within, "by": city })).await;
    assert_eq!(a["grouping"]["kind"], "members", "{a}");
    assert!(a["note"].as_str().unwrap().contains("levels deep"), "{a}");
    assert_eq!(a["members"].as_array().unwrap().len(), 14, "{a}");
}
