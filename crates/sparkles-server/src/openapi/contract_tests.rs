//! The description against real responses: a server with history, stored queries,
//! search indexes, reasoning, write-time validation and a backup answers the admin
//! routes, and each body must match its operation's `200` schema.

use super::*;
use crate::state::{AppState, DbType};
use axum::body::Body;
use axum::extract::Request;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// Why `v` does not match `schema` (`$ref`s resolve in `doc`), if it does not.
fn mismatch(doc: &J, schema: &J, v: &J, at: &str) -> Option<String> {
    let fail = |m: String| Some(format!("{at}: {m}"));
    if let Some(r) = schema.get("$ref").and_then(J::as_str) {
        let name = r.trim_start_matches("#/components/schemas/");
        let target = &doc["components"]["schemas"][name];
        assert!(!target.is_null(), "unresolved {r}");
        return mismatch(doc, target, v, at);
    }
    if let Some(c) = schema.get("const")
        && c != v
    {
        return fail(format!("{v} is not {c}"));
    }
    if let Some(e) = schema.get("enum").and_then(J::as_array)
        && !e.contains(v)
    {
        return fail(format!("{v} is not one of {e:?}"));
    }
    if let Some(t) = schema.get("type") {
        let types: Vec<&str> = match t {
            J::String(s) => vec![s.as_str()],
            J::Array(a) => a.iter().filter_map(J::as_str).collect(),
            _ => Vec::new(),
        };
        let ok = types.iter().any(|t| match *t {
            "null" => v.is_null(),
            "boolean" => v.is_boolean(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "string" => v.is_string(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            _ => false,
        });
        if !ok {
            return fail(format!("{v} is not of type {t}"));
        }
    }
    for s in schema
        .get("allOf")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(m) = mismatch(doc, s, v, at) {
            return Some(m);
        }
    }
    if let Some(any) = schema.get("anyOf").and_then(J::as_array) {
        let misses: Vec<String> = any.iter().filter_map(|s| mismatch(doc, s, v, at)).collect();
        if misses.len() == any.len() {
            return fail(format!("no anyOf branch matches: {misses:?}"));
        }
    }
    if let Some(one) = schema.get("oneOf").and_then(J::as_array) {
        let n = one
            .iter()
            .filter(|s| mismatch(doc, s, v, at).is_none())
            .count();
        if n != 1 {
            return fail(format!("{n} oneOf branches match {v}"));
        }
    }
    if let Some(o) = v.as_object() {
        for r in schema
            .get("required")
            .and_then(J::as_array)
            .into_iter()
            .flatten()
        {
            let r = r.as_str().unwrap();
            if !o.contains_key(r) {
                return fail(format!("the required member {r} is missing"));
            }
        }
        let props = schema.get("properties").and_then(J::as_object);
        for (k, x) in o {
            let at = format!("{at}.{k}");
            match props.and_then(|p| p.get(k)) {
                Some(s) => {
                    if let Some(m) = mismatch(doc, s, x, &at) {
                        return Some(m);
                    }
                }
                None => match schema.get("additionalProperties") {
                    Some(J::Bool(false)) => return fail(format!("{k} is not described")),
                    Some(s @ J::Object(_)) => {
                        if let Some(m) = mismatch(doc, s, x, &at) {
                            return Some(m);
                        }
                    }
                    _ => {}
                },
            }
        }
    }
    if let (Some(items), Some(a)) = (schema.get("items"), v.as_array()) {
        for (i, x) in a.iter().enumerate() {
            if let Some(m) = mismatch(doc, items, x, &format!("{at}[{i}]")) {
                return Some(m);
            }
        }
    }
    None
}

#[test]
fn the_validator_checks_what_it_reads() {
    let doc = json!({ "components": { "schemas": { "N": { "type": "integer" } } } });
    let s = json!({
        "type": "object",
        "required": ["a"],
        "properties": {
            "a": { "$ref": "#/components/schemas/N" },
            "b": { "oneOf": [{ "const": "all" }, { "type": "array", "items": { "type": "string" } }] },
            "c": { "type": ["string", "null"] },
        },
    });
    assert!(mismatch(&doc, &s, &json!({ "a": 1, "b": "all", "c": null }), "$").is_none());
    assert!(mismatch(&doc, &s, &json!({ "a": 1, "b": ["x"] }), "$").is_none());
    assert!(mismatch(&doc, &s, &json!({ "b": "all" }), "$").is_some());
    assert!(mismatch(&doc, &s, &json!({ "a": 1.5 }), "$").is_some());
    assert!(mismatch(&doc, &s, &json!({ "a": 1, "b": "some" }), "$").is_some());
    assert!(mismatch(&doc, &s, &json!({ "a": 1, "b": [1] }), "$").is_some());
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<J>) -> (StatusCode, J) {
    call_as(app, method, uri, body, None).await
}

/// `call` with an `Accept` header.
async fn call_as(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<J>,
    accept: Option<&str>,
) -> (StatusCode, J) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(a) = accept {
        req = req.header("accept", a);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let j = serde_json::from_slice(&bytes).unwrap_or(J::Null);
    (status, j)
}

/// Send `method uri` with a JSON `body`, and check the body against the operation's JSON
/// request schema and the answer against the JSON schema of its status, which must be a
/// success. The answer's body.
async fn checked(
    doc: &J,
    app: &axum::Router,
    method: &str,
    template: &str,
    uri: &str,
    body: Option<J>,
) -> J {
    let op = &doc["paths"][template][method.to_ascii_lowercase()];
    assert!(!op.is_null(), "no {method} {template}");
    if let Some(b) = &body {
        let sch = &op["requestBody"]["content"]["application/json"]["schema"];
        assert!(
            !sch.is_null(),
            "{method} {template} has no JSON request schema"
        );
        if let Some(m) = mismatch(doc, sch, b, "$") {
            panic!("the request of {method} {uri} does not match its schema: {m}\n{b}");
        }
    }
    let (s, j) = call_as(app, method, uri, body, Some("application/json")).await;
    assert!(s.is_success(), "{method} {uri}: {s} {j}");
    let mut resp = &op["responses"][s.as_str()];
    if let Some(r) = resp.get("$ref").and_then(J::as_str) {
        resp = &doc["components"]["responses"][r.trim_start_matches("#/components/responses/")];
    }
    let sch = &resp["content"]["application/json"]["schema"];
    assert!(!sch.is_null(), "{method} {template} has no {s} JSON schema");
    if let Some(m) = mismatch(doc, sch, &j, "$") {
        panic!("{method} {uri} does not match the {s} schema of {template}: {m}\n{j}");
    }
    j
}

/// Poll `uri` until `done` holds for its body.
async fn until(app: &axum::Router, uri: &str, done: impl Fn(&J) -> bool) {
    for _ in 0..3000 {
        let (s, j) = call(app, "GET", uri, None).await;
        if s == StatusCode::OK && done(&j) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{uri} did not get ready");
}

#[tokio::test]
async fn admin_responses_match_their_schemas() {
    let dir = tempfile::tempdir().unwrap();
    // mutable for the backup state, in builds that have it
    #[allow(unused_mut)]
    let mut st = AppState::new(
        &dir.path().join("data"),
        Default::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    #[cfg(feature = "backup")]
    {
        st.backup = Some(Arc::new(
            crate::backup::BackupState::new(&dir.path().join("data"), None, 2).unwrap(),
        ));
    }
    let st = Arc::new(st);
    st.attach("h", DbType::Persistent, None).unwrap();
    let app = crate::http::router(st.clone());
    let update = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
        PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        INSERT DATA { ex:a a rdfs:Class ; rdfs:label \"alpha\"@en ; ex:emb \"[1,0,0]\"^^<urn:x-sparkles:vector> . \
        ex:b rdfs:subClassOf ex:a ; rdfs:label \"beta\" . ex:x a ex:b ; geo:hasGeometry ex:gx . \
        ex:gx geo:asWKT \"POINT(1 2)\"^^geo:wktLiteral }";
    let res = app
        .clone()
        .oneshot(
            Request::post("/h/update")
                .header("content-type", "application/sparql-update")
                .body(Body::from(update))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success());
    // the routes to check, after the features they report on are set up
    #[allow(unused_mut)]
    let mut routes: Vec<(&str, &str)> = vec![
        ("/$/history/{ds}", "/$/history/h"),
        ("/$/snapshots/{ds}", "/$/snapshots/h"),
        ("/$/snapshots/{ds}/{name}", "/$/snapshots/h/v1"),
        ("/{ds}/history", "/h/history?subject=http://example.org/a"),
        ("/$/queries/{ds}", "/$/queries/h"),
        ("/$/queries/{ds}/{name}", "/$/queries/h/labels"),
        ("/$/vector/{ds}", "/$/vector/h"),
        ("/$/vector/{ds}/{name}", "/$/vector/h/v1"),
        ("/$/validation/{ds}", "/$/validation/h"),
    ];
    let put = |uri: &'static str, body: J| {
        let app = app.clone();
        async move {
            let (s, j) = call(&app, "PUT", uri, Some(body)).await;
            assert!(s.is_success(), "{uri}: {s} {j}");
        }
    };
    let post = |uri: &'static str, body: J| {
        let app = app.clone();
        async move {
            let (s, j) = call(&app, "POST", uri, Some(body)).await;
            assert!(s.is_success(), "{uri}: {s} {j}");
        }
    };
    // validation is off first: {config: null}
    let (_, j) = call(&app, "GET", "/$/validation/h", None).await;
    let doc = build("0.0.0");
    let schema = |template: &str| {
        doc["paths"][template]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            .clone()
    };
    assert_eq!(mismatch(&doc, &schema("/$/validation/{ds}"), &j, "$"), None);
    put(
        "/$/queries/h/labels",
        json!({ "query": "SELECT ?s ?l WHERE { ?s <http://www.w3.org/2000/01/rdf-schema#label> ?l }", "description": "labels" }),
    )
    .await;
    post("/$/snapshots/h", json!({ "name": "v1", "note": "first" })).await;
    put(
        "/$/vector/h/v1",
        json!({ "predicate": "http://example.org/emb", "dimension": 3 }),
    )
    .await;
    until(&app, "/$/vector/h/v1", |j| j["state"] == "ready").await;
    put(
        "/$/validation/h",
        json!({ "mode": "warn", "shapes": { "inline": "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://example.org/> . ex:S a sh:NodeShape ; sh:targetClass ex:Thing ; sh:property [ sh:path ex:p ; sh:maxCount 1 ] ." } }),
    )
    .await;
    #[cfg(feature = "text")]
    {
        put("/$/text/h", json!({ "languages": ["en", "ja"] })).await;
        until(&app, "/$/text/h", |j| j["state"] == "ready").await;
        routes.push(("/$/text/{ds}", "/$/text/h"));
    }
    #[cfg(feature = "geo")]
    {
        put("/$/geo/h", json!({})).await;
        until(&app, "/$/geo/h", |j| j["state"] == "ready").await;
        routes.push(("/$/geo/{ds}", "/$/geo/h"));
    }
    #[cfg(feature = "reasoning")]
    {
        post("/$/reason/h", json!({ "profile": "rdfs" })).await;
        until(&app, "/$/reason/h", |j| j["inferred"].as_u64() > Some(0)).await;
        routes.push(("/$/reason/{ds}", "/$/reason/h"));
    }
    #[cfg(feature = "backup")]
    {
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let (s, j) = call(
            &app,
            "POST",
            "/$/repositories",
            Some(json!({ "name": "r1", "type": "fs", "path": repo })),
        )
        .await;
        assert!(s.is_success(), "{s} {j}");
        assert_eq!(
            mismatch(&doc, &sref("Repository"), &j, "$"),
            None,
            "the created repository"
        );
        post("/$/backups/h", json!({ "repository": "r1", "note": "n" })).await;
        until(&app, "/$/backups/h", |j| {
            j["backups"].as_array().is_some_and(|b| !b.is_empty())
        })
        .await;
        let (_, list) = call(&app, "GET", "/$/backups/h", None).await;
        let name = list["backups"][0]["name"].as_str().unwrap().to_string();
        let one = format!("/$/backups/h/r1/{name}");
        let (s, j) = call(&app, "GET", &one, None).await;
        assert_eq!(s, StatusCode::OK, "{j}");
        assert_eq!(
            mismatch(&doc, &schema("/$/backups/{ds}/{repo}/{backup}"), &j, "$"),
            None
        );
        routes.extend([
            ("/$/repositories", "/$/repositories"),
            ("/$/repositories/{repo}", "/$/repositories/r1"),
            ("/$/backups/{ds}", "/$/backups/h"),
            (
                "/$/repositories/{repo}/backups",
                "/$/repositories/r1/backups",
            ),
        ]);
    }
    more_answers(&doc, &app).await;
    for (template, uri) in routes {
        let (s, j) = call(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::OK, "{uri}: {j}");
        let sch = schema(template);
        assert!(!sch.is_null(), "{template} has no 200 JSON schema");
        if let Some(m) = mismatch(&doc, &sch, &j, "$") {
            panic!("{uri} does not match the schema of {template}: {m}\n{j}");
        }
    }
}

/// Wait for a task to end, and require that it did its work.
#[cfg(feature = "backup")]
async fn finished(app: &axum::Router, task: &J) {
    let id = task["id"].as_str().expect("a task id");
    let uri = format!("/$/tasks/{id}");
    for _ in 0..3000 {
        let (s, j) = call(app, "GET", &uri, None).await;
        assert_eq!(s, StatusCode::OK, "{uri}: {j}");
        match j["state"].as_str() {
            Some("done") => return,
            Some("failed" | "cancelled") => panic!("{uri}: {j}"),
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    panic!("{uri} did not finish");
}

/// The answers of the routes beyond the admin `GET`s: Fuseki's services, compaction,
/// diffs, the change feed, write previews and the feature routes, with their requests.
async fn more_answers(doc: &J, app: &axum::Router) {
    let c = |m: &'static str, t: &'static str, u: &'static str, b: Option<J>| {
        let (doc, app) = (doc.clone(), app.clone());
        async move { checked(&doc, &app, m, t, u, b).await }
    };
    c("GET", "/$/metrics", "/$/metrics?format=json", None).await;
    c("GET", "/$/stats", "/$/stats", None).await;
    c("GET", "/$/stats/{ds}", "/$/stats/h", None).await;
    c("GET", "/$/backups-list", "/$/backups-list", None).await;
    for (t, u) in [
        (
            "/$/validate/query",
            "/$/validate/query?query=SELECT%20*%20WHERE%20%7B%7D",
        ),
        ("/$/validate/query", "/$/validate/query?query=SELEC"),
        (
            "/$/validate/update",
            "/$/validate/update?update=CLEAR%20ALL",
        ),
        ("/$/validate/iri", "/$/validate/iri?iri=http://e/x&iri=rel"),
        (
            "/$/validate/data",
            "/$/validate/data?data=%3Curn:a%3E%20%3Curn:b%3E%20.",
        ),
        (
            "/$/validate/langtag",
            "/$/validate/langtag?langtag=en-Latn-US&langtag=en%20US",
        ),
    ] {
        c("GET", t, u, None).await;
    }
    c("GET", "/$/compaction/{ds}", "/$/compaction/h", None).await;
    c(
        "PUT",
        "/$/compaction/{ds}",
        "/$/compaction/h",
        Some(json!({ "deltaRatio": 0.5, "partial": "off" })),
    )
    .await;
    let j = c("GET", "/{ds}/diff", "/h/diff?from=0&quads=true", None).await;
    assert!(j["added"].as_u64() > Some(0), "{j}");
    let j = c("GET", "/{ds}/changes", "/h/changes?after=0", None).await;
    assert!(!j["commits"].as_array().unwrap().is_empty(), "{j}");
    c(
        "GET",
        "/$/queries/{ds}/{name}/versions",
        "/$/queries/h/labels/versions",
        None,
    )
    .await;
    c(
        "POST",
        "/$/vector/{ds}/{name}/recall",
        "/$/vector/h/v1/recall?samples=1&k=1",
        None,
    )
    .await;
    c(
        "PUT",
        "/$/history/{ds}",
        "/$/history/h",
        Some(json!({
            "keepCommits": 100, "keepAge": "7d",
            "schedules": [{ "prefix": "daily", "every": "1d", "keepLast": 3 }],
            "catalog": { "keepCommits": 1000 },
            "changeLog": { "enabled": true, "maxBytes": "1GiB" },
        })),
    )
    .await;
    c(
        "POST",
        "/$/snapshots/{ds}",
        "/$/snapshots/h",
        Some(json!({ "name": "v2", "at": 1, "note": "n", "expires": "1d", "warm": false })),
    )
    .await;
    c("GET", "/$/rdfs/{ds}", "/$/rdfs/h", None).await;
    let j = c(
        "PUT",
        "/$/rdfs/{ds}",
        "/$/rdfs/h",
        Some(json!({ "graph": "default" })),
    )
    .await;
    assert_eq!(j["enabled"], true, "{j}");
    c("DELETE", "/$/rdfs/{ds}", "/$/rdfs/h", None).await;
    // a dry run answers in JSON whatever the update's media type
    let res = app
        .clone()
        .oneshot(
            Request::post("/h/update?dryRun=true&changes=5")
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.org/c> <http://example.org/p> 1 }",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let j: J = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(mismatch(doc, &sref("DryRunReport"), &j, "$"), None, "{j}");
    assert!(j["validation"].is_object(), "{j}");
    #[cfg(feature = "shacl")]
    {
        let j = c(
            "GET",
            "/$/schema/{ds}/constraints",
            "/$/schema/h/constraints?shapes=guard",
            None,
        )
        .await;
        assert_eq!(j["constraints"]["sources"][0]["kind"], "guard", "{j}");
    }
    #[cfg(feature = "reasoning")]
    {
        c(
            "GET",
            "/$/reason/{ds}/diagnostics",
            "/$/reason/h/diagnostics",
            None,
        )
        .await;
        c(
            "PUT",
            "/$/reason/{ds}/auto",
            "/$/reason/h/auto",
            Some(json!({ "enabled": true, "debounceSeconds": 5 })),
        )
        .await;
    }
    #[cfg(feature = "text")]
    {
        let j = c("GET", "/{ds}/text", "/h/text?q=alpha&highlight=true", None).await;
        assert!(!j["hits"].as_array().unwrap().is_empty(), "{j}");
    }
    #[cfg(feature = "geo")]
    c(
        "POST",
        "/$/geo/convert",
        "/$/geo/convert",
        Some(json!({ "literals": [
            { "value": "POINT(1 2)", "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral" },
            { "value": "POINT(", "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral" },
        ] })),
    )
    .await;
    #[cfg(feature = "shex")]
    {
        let envelope = json!({
            "schema": "PREFIX ex: <http://example.org/> ex:S { ex:p . }",
            "map": "<http://example.org/x>@<http://example.org/S>",
        });
        let j = c("POST", "/{ds}/shex", "/h/shex", Some(envelope.clone())).await;
        assert_eq!(j["conforms"], false, "{j}");
        let j = c(
            "POST",
            "/{ds}/shex",
            "/h/shex?format=shapemap",
            Some(envelope),
        )
        .await;
        assert!(j.is_array(), "{j}");
    }
    #[cfg(feature = "backup")]
    backup_answers(doc, app).await;
}

/// The backup routes beyond listings: requests that start tasks, locks and policies.
#[cfg(feature = "backup")]
async fn backup_answers(doc: &J, app: &axum::Router) {
    let c = |m: &'static str, t: &'static str, u: &'static str, b: Option<J>| {
        let (doc, app) = (doc.clone(), app.clone());
        async move { checked(&doc, &app, m, t, u, b).await }
    };
    let t = c(
        "POST",
        "/$/backups/{ds}",
        "/$/backups/h",
        Some(json!({ "repository": "r1", "name": "b2", "note": "second" })),
    )
    .await;
    finished(app, &t).await;
    let t = c(
        "POST",
        "/$/backups/{ds}/{repo}/{backup}/verify",
        "/$/backups/h/r1/b2/verify",
        Some(json!({ "level": "data" })),
    )
    .await;
    finished(app, &t).await;
    let t = c(
        "POST",
        "/$/repositories/{repo}/gc",
        "/$/repositories/r1/gc",
        Some(json!({ "dryRun": true, "graceHours": 1 })),
    )
    .await;
    finished(app, &t).await;
    c(
        "GET",
        "/$/repositories/{repo}/locks",
        "/$/repositories/r1/locks",
        None,
    )
    .await;
    let t = c(
        "POST",
        "/$/backups/{ds}/{repo}/{backup}/restore",
        "/$/backups/h/r1/b2/restore",
        Some(json!({ "target": "h2", "identity": "new", "check": "quick" })),
    )
    .await;
    finished(app, &t).await;
    c(
        "POST",
        "/$/backup-policies",
        "/$/backup-policies",
        Some(json!({
            "name": "p1", "repository": "r1", "datasets": ["h"], "schedule": "every 1h",
            "retention": { "expireAfter": null, "minCount": 1, "maxCount": 5 },
        })),
    )
    .await;
    c(
        "POST",
        "/$/backup-policies/preview",
        "/$/backup-policies/preview",
        Some(json!({
            "schedule": "0 2 * * *", "timezone": "Europe/Berlin", "count": 2,
            "nameTemplate": "{policy}-{dataset}", "dataset": "h",
        })),
    )
    .await;
    let t = c(
        "POST",
        "/$/backup-policies/{policy}/run",
        "/$/backup-policies/p1/run",
        None,
    )
    .await;
    finished(app, &t).await;
    let j = c(
        "GET",
        "/$/backup-policies/{policy}/runs",
        "/$/backup-policies/p1/runs",
        None,
    )
    .await;
    assert!(!j["runs"].as_array().unwrap().is_empty(), "{j}");
    c(
        "POST",
        "/$/backup-policies/{policy}/retention",
        "/$/backup-policies/p1/retention?dryRun=true",
        None,
    )
    .await;
    c(
        "PUT",
        "/$/backup-policies/{policy}",
        "/$/backup-policies/p1",
        Some(json!({ "repository": "r1", "schedule": "every 2h", "enabled": false })),
    )
    .await;
    let j = c(
        "GET",
        "/$/backup-policies/{policy}",
        "/$/backup-policies/p1",
        None,
    )
    .await;
    assert!(j["state"]["lastRun"].is_object(), "{j}");
    c("GET", "/$/backup-policies", "/$/backup-policies", None).await;
}
