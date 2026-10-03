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
    let mut req = Request::builder().method(method).uri(uri);
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
