//! The GraphRAG routes against a stub model provider (C22 §2.2, §2.3, §4).

use super::super::build::tests::{ORG, fixture, patch};
use super::super::guard::gr;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(
            b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn enable(app: &axum::Router) {
    let s = json!({
        "enabled": true,
        "principal": "reports",
        "projection": { "graphs": [ORG] },
        "levels": 1,
        "minCommunitySize": 2
    });
    assert_eq!(
        patch(app, "/$/settings/org/graphrag", s).await,
        StatusCode::OK
    );
}

/// Wait for a task to leave `running`, and return it.
async fn finished(app: &axum::Router, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, t) = call(app, "GET", &format!("/$/tasks/{id}"), None).await;
        let state = t["state"].as_str().unwrap_or("").to_string();
        if state != "running" && state != "queued" {
            return t;
        }
        assert!(Instant::now() < deadline, "the task never finished: {t}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_runs_as_a_task_and_its_reports_can_be_read_and_reviewed() {
    let f = fixture(4);
    f.allow_rows().await;
    // not enabled yet
    let (s, b) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/build",
        Some(json!({ "confirm": true })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "not-enabled", "{b}");
    enable(&f.app).await;
    let (s, task) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/build",
        Some(json!({ "confirm": true })),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["kind"], "graphrag");
    let t = finished(&f.app, task["id"].as_str().unwrap()).await;
    assert_eq!(t["state"], "done", "{t}");
    assert_eq!(t["detail"]["written"], 2, "{t}");

    let (s, st) = call(&f.app, "GET", "/$/graphrag/org", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["available"], true, "{st}");
    assert_eq!(st["reports"]["unreviewed"], 2, "{st}");
    assert_eq!(st["levels"][0]["communities"], 2, "{st}");
    assert!(st["lastBuild"]["sourceCommit"].is_u64(), "{st}");
    assert!(st["budget"]["usedToday"].as_u64().unwrap() > 0, "{st}");

    let (_, list) = call(&f.app, "GET", "/$/graphrag/org/communities", None).await;
    let cs = list["communities"].as_array().unwrap();
    assert_eq!(cs.len(), 2, "{list}");
    let c = cs[0]["iri"].as_str().unwrap().to_string();
    let rep = cs[0]["report"]["iri"].as_str().unwrap().to_string();
    assert_eq!(cs[0]["report"]["title"], "A community");

    let uri = format!(
        "/$/graphrag/org/community?iri={}",
        percent_encoding::utf8_percent_encode(&c, percent_encoding::NON_ALPHANUMERIC)
    );
    let (s, one) = call(&f.app, "GET", &uri, None).await;
    assert_eq!(s, StatusCode::OK, "{one}");
    assert_eq!(one["memberCount"], 4, "{one}");
    assert_eq!(one["members"].as_array().unwrap().len(), 4);
    let findings = one["report"]["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{one}");
    assert_eq!(findings[0]["cites"].as_array().unwrap().len(), 1, "{one}");
    assert_eq!(one["report"]["activity"]["model"], "p/m", "{one}");
    let (s, _) = call(
        &f.app,
        "GET",
        "/$/graphrag/org/community?iri=urn:nope",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, inbox) = call(&f.app, "GET", "/$/memory/org/inbox", None).await;
    assert_eq!(inbox["communities"]["unreviewed"], 2, "{inbox}");
    assert_eq!(
        inbox["communities"]["reports"].as_array().map(Vec::len),
        Some(2),
        "{inbox}"
    );
    // a status review
    let (s, r) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/review",
        Some(json!({ "report": rep, "status": "rejected" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (_, st) = call(&f.app, "GET", "/$/graphrag/org", None).await;
    assert_eq!(st["reports"]["rejected"], 1, "{st}");
    assert_eq!(st["reports"]["unreviewed"], 1, "{st}");
    let (s, _) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/review",
        Some(json!({ "report": rep, "status": "stale" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // an edit supersedes the report, keeps its findings and is reviewed
    let (s, r) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/review",
        Some(json!({ "report": rep, "title": "The x team" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (_, one) = call(&f.app, "GET", &uri, None).await;
    let new = &one["report"];
    assert_ne!(new["iri"], rep.as_str(), "{one}");
    assert_eq!(new["title"], "The x team");
    assert_eq!(new["summary"], "People who know each other.");
    assert_eq!(new["status"], "reviewed");
    assert_eq!(new["revisionOf"], rep.as_str());
    assert_eq!(new["findings"].as_array().unwrap().len(), 1, "{one}");
    assert!(
        new["activity"].is_null(),
        "a person's edit has no model activity: {one}"
    );
    let (_, st) = call(&f.app, "GET", "/$/graphrag/org", None).await;
    assert_eq!(st["reports"]["reviewed"], 1, "{st}");
    assert_eq!(
        st["reports"]["rejected"], 0,
        "the superseded report is not current: {st}"
    );
    // the new report's finding is its own, so the old report can go without it
    let own = f.rows(&format!(
        "SELECT ?f WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ <{}> <{}> ?f FILTER(STRSTARTS(STR(?f), \"{}#\")) }} }}",
        new["iri"].as_str().unwrap(),
        gr("finding"),
        new["iri"].as_str().unwrap(),
    ));
    assert_eq!(own.len(), 1);

    // the inbox lists the report that waits, and the metrics count the statuses
    let (s, inbox) = call(&f.app, "GET", "/$/memory/org/inbox", None).await;
    assert_eq!(s, StatusCode::OK, "{inbox}");
    let c = &inbox["communities"];
    assert_eq!(c["available"], true, "{inbox}");
    assert_eq!(
        c["unreviewed"], 1,
        "the other community still waits: {inbox}"
    );
    assert_eq!(c["anomalies"], json!([]), "{inbox}");
    let m = crate::obs::render_prometheus(&f.st);
    assert!(
        m.contains("sparkles_graphrag_reports{dataset=\"org\",status=\"reviewed\"} 1"),
        "{m}"
    );
    assert!(
        m.contains("sparkles_graphrag_tokens_total{dataset=\"org\",phase=\"build\"}"),
        "{m}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_waits_for_its_confirmation() {
    let f = fixture(3);
    f.allow_rows().await;
    enable(&f.app).await;
    let (s, task) = call(&f.app, "POST", "/$/graphrag/org/build", Some(json!({}))).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{task}");
    let id = task["id"].as_str().unwrap().to_string();
    // a second build of the dataset is refused while the first runs
    let (s, _) = call(&f.app, "POST", "/$/graphrag/org/refresh", Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/confirm",
        Some(json!({ "task": "nope" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // the estimate shows in the task while it waits
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, t) = call(&f.app, "GET", &format!("/$/tasks/{id}"), None).await;
        if t["detail"]["phase"] == "awaiting-confirmation" {
            assert!(t["detail"]["estimate"].is_object(), "{t}");
            break;
        }
        assert!(Instant::now() < deadline, "{t}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(f.mock.requests().is_empty());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (s, _) = call(
            &f.app,
            "POST",
            "/$/graphrag/org/confirm",
            Some(json!({ "task": id })),
        )
        .await;
        if s == StatusCode::OK {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let t = finished(&f.app, &id).await;
    assert_eq!(t["state"], "done", "{t}");
    assert!(!f.mock.requests().is_empty());
}

/// The record of the last run describes the graph that run wrote: once the settings
/// name another reports graph, the status leaves it out (round 6, finding 6).
#[tokio::test(flavor = "multi_thread")]
async fn the_last_run_shows_only_for_its_own_graph() {
    let f = fixture(3);
    f.allow_rows().await;
    enable(&f.app).await;
    let (_, task) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/build",
        Some(json!({ "confirm": true })),
    )
    .await;
    let t = finished(&f.app, task["id"].as_str().unwrap()).await;
    assert_eq!(t["state"], "done", "{t}");
    let (_, st) = call(&f.app, "GET", "/$/graphrag/org", None).await;
    assert!(st["lastBuild"].is_object(), "{st}");
    assert_eq!(
        patch(
            &f.app,
            "/$/settings/org/graphrag",
            json!({ "reportsGraph": "urn:x-sparkles:graphrag:other" })
        )
        .await,
        StatusCode::OK
    );
    let (_, st) = call(&f.app, "GET", "/$/graphrag/org", None).await;
    assert!(st.get("lastBuild").is_none(), "{st}");
}

/// The Explore overview groups the dataset by the hierarchy a build wrote (U06 with C22
/// §4): its groups are the stored communities of the level with their report titles,
/// statuses and member counts, the links between groups are the data's links between
/// their members, and each community opens into its members. The settings' reports graph
/// is the one read.
#[tokio::test(flavor = "multi_thread")]
async fn the_overview_groups_the_dataset_by_a_built_hierarchy() {
    let f = fixture(4);
    f.allow_rows().await;
    enable(&f.app).await;
    let (_, task) = call(
        &f.app,
        "POST",
        "/$/graphrag/org/build",
        Some(json!({ "confirm": true })),
    )
    .await;
    let t = finished(&f.app, task["id"].as_str().unwrap()).await;
    assert_eq!(t["state"], "done", "{t}");
    let requests = f.mock.requests().len();

    let (_, list) = call(&f.app, "GET", "/$/graphrag/org/communities", None).await;
    let stored: Vec<&Value> = list["communities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["level"] == 0)
        .collect();
    assert_eq!(stored.len(), 2, "{list}");
    let (s, top) = call(
        &f.app,
        "POST",
        "/$/graph/org/groups",
        Some(json!({ "by": { "reports": {} } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{top}");
    assert_eq!(top["available"], true, "{top}");
    assert_eq!(
        top["within"]["reports"]["graph"],
        "urn:x-sparkles:graphrag:reports"
    );
    assert_eq!(top["within"]["size"], 8, "{top}");
    let groups = top["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{top}");
    let mut members_of: Vec<(String, Vec<String>)> = Vec::new();
    for g in groups {
        let c = stored
            .iter()
            .find(|c| c["iri"] == g["community"])
            .unwrap_or_else(|| panic!("{g} is not a stored community: {list}"));
        assert_eq!(g["size"], c["memberCount"], "{g}");
        assert_eq!(g["label"], c["report"]["title"], "{g}");
        assert_eq!(g["report"]["status"], c["report"]["status"], "{g}");
        assert_eq!(g["report"]["status"], "unreviewed", "{g}");
        let (s, m) = call(
            &f.app,
            "POST",
            "/$/graph/org/groups",
            Some(json!({ "within": { "group": g["id"] } })),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{m}");
        assert_eq!(m["grouping"]["kind"], "members", "{m}");
        let iris: Vec<String> = m["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["iri"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(iris.len() as u64, g["size"].as_u64().unwrap(), "{m}");
        members_of.push((format!("g:{}", g["id"].as_str().unwrap()), iris));
    }
    // each cluster is a community, and x0 knows y0 between them
    let cluster = |iris: &[String]| iris[0].rsplit('/').next().unwrap()[..1].to_string();
    for (_, iris) in &members_of {
        let c = cluster(iris);
        assert!(
            iris.iter().all(|i| cluster(std::slice::from_ref(i)) == c),
            "{iris:?}"
        );
    }
    let between: Vec<(&str, &str, u64)> = top["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["source"] != e["target"])
        .map(|e| {
            (
                e["source"].as_str().unwrap(),
                e["target"].as_str().unwrap(),
                e["count"].as_u64().unwrap(),
            )
        })
        .collect();
    let x = members_of.iter().find(|(_, m)| cluster(m) == "x").unwrap();
    let y = members_of.iter().find(|(_, m)| cluster(m) == "y").unwrap();
    assert_eq!(between, [(x.0.as_str(), y.0.as_str(), 1)], "{top}");
    // six links inside each cluster of four
    for (key, _) in &members_of {
        let inside = top["edges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["source"] == key.as_str() && e["target"] == key.as_str())
            .unwrap();
        assert_eq!(inside["count"], 6, "{top}");
    }
    // the overview reads triples only: no model request
    assert_eq!(f.mock.requests().len(), requests);

    // another reports graph in the settings holds no communities yet
    assert_eq!(
        patch(
            &f.app,
            "/$/settings/org/graphrag",
            json!({ "reportsGraph": "urn:x-sparkles:graphrag:other" })
        )
        .await,
        StatusCode::OK
    );
    let (_, other) = call(
        &f.app,
        "POST",
        "/$/graph/org/groups",
        Some(json!({ "by": { "reports": {} } })),
    )
    .await;
    assert_eq!(other["available"], false, "{other}");
    assert_eq!(other["unavailable"], "none", "{other}");
}
