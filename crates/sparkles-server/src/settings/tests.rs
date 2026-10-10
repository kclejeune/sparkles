//! Layered settings (spec C19): the resolution of §4 and the routes of §6, with the
//! acceptance examples that run in the process. `tests/cli_settings.rs` runs the ones
//! that need a server process: the start, SIGHUP and a dataset of `--loc`.

use super::*;
use crate::state::DbType;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn p(s: &str) -> Vec<String> {
    merge::parse_path(s).unwrap()
}

fn declared(v: Value) -> Declared {
    Declared::parse(&v.to_string(), Providers::Unchecked).unwrap()
}

fn src(r: &Resolved, field: &str) -> &'static str {
    r.sources[&p(field)]
}

#[test]
fn layers_and_sources() {
    let d = declared(json!({
        "defaults": {"assistant": {"send": "rows", "budget": {"perRequest": 1000}}},
        "datasets": {"slurp": {"assistant": {"enabled": true, "historyDays": 30}}}
    }));
    let rt = json!({"historyDays": 7, "budget": {"perPrincipalPerDay": 5}});
    let r = resolve(&ASSISTANT, &d, "slurp", rt, Providers::Unchecked);
    assert_eq!(r.status, Ok(()));
    assert_eq!(r.effective["enabled"], true);
    assert_eq!(r.effective["send"], "rows");
    assert_eq!(r.effective["historyDays"], 7);
    assert_eq!(
        r.effective["budget"],
        json!({"perRequest": 1000, "perPrincipalPerDay": 5})
    );
    assert_eq!(src(&r, "enabled"), "declared");
    assert_eq!(src(&r, "send"), "declared");
    assert_eq!(src(&r, "historyDays"), "runtime");
    assert_eq!(src(&r, "budget.perRequest"), "declared");
    assert_eq!(src(&r, "budget.perPrincipalPerDay"), "runtime");
    assert_eq!(src(&r, "ask"), "default");
    assert_eq!(
        r.declared,
        json!({"send": "rows", "budget": {"perRequest": 1000}, "enabled": true, "historyDays": 30})
    );
    // another dataset gets the defaults only
    let r = resolve(&ASSISTANT, &d, "other", json!({}), Providers::Unchecked);
    assert_eq!(r.effective["enabled"], false);
    assert_eq!(r.effective["send"], "rows");
}

#[test]
fn locks_fix_fields_and_mark_runtime_values() {
    let d = declared(json!({
        "defaults": {"assistant": {"send": "schema"}, "locked": ["assistant.send"]},
        "datasets": {"slurp": {"assistant": {"send": "rows"}}}
    }));
    // a dataset entry that sets a locked field keeps it locked at its value
    let r = resolve(
        &ASSISTANT,
        &d,
        "slurp",
        json!({"send": "documents"}),
        Providers::Unchecked,
    );
    assert_eq!(r.effective["send"], "rows");
    assert_eq!(src(&r, "send"), "locked");
    assert_eq!(r.overridden, vec![p("send")]);
    let r = resolve(&ASSISTANT, &d, "x", json!({}), Providers::Unchecked);
    assert_eq!(r.effective["send"], "schema");
    assert!(r.overridden.is_empty());
}

/// `overrides` names each runtime value that is used in place of a different declared
/// value, and leaves out values equal to the declared one, values with no declared
/// value beneath them and locked fields.
#[test]
fn overrides_name_the_declared_values_a_runtime_value_replaces() {
    let d = declared(json!({
        "defaults": {
            "assistant": {"send": "schema", "budget": {"perRequest": 1000}, "historyDays": 30},
            "locked": ["assistant.send"]
        },
        "datasets": {"slurp": {"assistant": {"sendByProvider": {"claude": "schema"}}}}
    }));
    let rt = json!({
        "historyDays": 7,
        "send": "rows",
        "explain": true,
        "budget": {"perRequest": 1000, "perPrincipalPerDay": 5},
        "sendByProvider": {"claude": "rows", "local": "rows"}
    });
    let r = resolve(&ASSISTANT, &d, "slurp", rt, Providers::Unchecked);
    let got: Vec<(String, Value, Value)> = r
        .overrides
        .iter()
        .map(|o| {
            (
                merge::path_string(&o.path),
                o.declared.clone(),
                o.runtime.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("historyDays".into(), json!(30), json!(7)),
            (
                "sendByProvider.claude".into(),
                json!("schema"),
                json!("rows")
            ),
        ]
    );
    let j = r.json("slurp");
    assert_eq!(
        j["overrides"][0],
        json!({"path": "historyDays", "declared": 30, "runtime": 7})
    );
    // the sources are unchanged
    assert_eq!(j["sources"]["historyDays"], "runtime");
    // nothing at runtime, nothing overridden
    let r = resolve(&ASSISTANT, &d, "slurp", json!({}), Providers::Unchecked);
    assert!(r.overrides.is_empty());
    assert_eq!(r.json("slurp")["overrides"], json!([]));
    // a runtime object in place of a declared scalar is reported at the scalar
    let o = overrides_of(&json!({"a": 5}), &json!({"a": {"x": 1}}), &[]);
    assert_eq!(
        o,
        vec![Override {
            path: p("a"),
            declared: json!(5),
            runtime: json!({"x": 1})
        }]
    );
}

/// A6: a file written before layering is a complete runtime layer and gives the same
/// effective object as before.
#[test]
fn a_legacy_file_reads_as_before() {
    let legacy = AssistantSettings {
        enabled: true,
        ingest: true,
        history_days: Some(9),
        ..Default::default()
    };
    let v = serde_json::to_value(&legacy).unwrap();
    let r = resolve(
        &ASSISTANT,
        &Declared::default(),
        "a",
        v,
        Providers::Unchecked,
    );
    let back: AssistantSettings = r.typed("a");
    assert_eq!(back, legacy);
    assert!(
        r.sources.values().all(|s| *s == "runtime"),
        "{:?}",
        r.sources
    );
    let m = crate::assist::MemorySettings {
        agent_graphs: vec!["urn:x:*".into()],
        ..Default::default()
    };
    let r = resolve(
        &MEMORY,
        &Declared::default(),
        "a",
        serde_json::to_value(&m).unwrap(),
        Providers::Unchecked,
    );
    assert_eq!(r.typed::<crate::assist::MemorySettings>("a"), m);
}

use crate::assistant::AssistantSettings;

/// A4 offline: a value the kind does not take, members that name endpoints or keys,
/// unknown kinds and malformed locks fail the file.
#[test]
fn the_settings_file_is_checked() {
    let bad = |v: Value| Declared::parse(&v.to_string(), Providers::Unchecked).unwrap_err();
    let e = bad(json!({"datasets": {"slurp": {"assistant": {"send": "everything"}}}}));
    assert!(e.starts_with("datasets.slurp.assistant:"), "{e}");
    assert!(bad(json!({"defaults": {"assistant": {"roles": {"draft": [{"provider": "a", "model": "m", "endpoint": "http://x"}]}}}})).contains("endpoint"));
    assert!(bad(json!({"defaults": {"apiKey": "k"}})).contains("apiKey"));
    assert!(bad(json!({"defaults": {"assistent": {}}})).contains("unknown settings kind"));
    assert!(bad(json!({"defaults": {"locked": ["assistant"]}})).contains("locked"));
    assert!(bad(json!({"defaults": {"locked": ["assistant.sned"]}})).contains("no member"));
    assert!(
        bad(json!({"defaults": {"memory": {"agentGraphs": ["not an iri"]}}}))
            .contains("defaults.memory")
    );
    assert!(bad(json!({"other": {}})).contains("unknown member"));
    assert!(bad(json!({"server": {"locked": ["assistant.send"]}})).contains("server"));
    // locks of server-wide settings are kept for their kinds
    let d = declared(
        json!({"server": {"locked": ["models.providers.claude.endpoint", "secrets.anthropic"]}}),
    );
    assert_eq!(d.server_locked.len(), 2);
    // with a model configuration, roles must name its providers
    assert!(
        Declared::parse(
            &json!({"defaults": {"assistant": {"roles": {"draft": [{"provider": "nope", "model": "m"}]}}}}).to_string(),
            Providers::Checked(None)
        )
        .is_err()
    );
    assert!(
        Declared::parse(
            &json!({"defaults": {"assistant": {"roles": {"draft": [{"provider": "nope", "model": "m"}]}}}}).to_string(),
            Providers::Unchecked
        )
        .is_ok()
    );
}

// ------------------------------------------------------------------ routes ------

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let etag = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null), etag)
}

fn req(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder().method(method).uri(uri);
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn state(dir: &std::path::Path, settings: Option<&std::path::Path>) -> Arc<AppState> {
    let mut st = AppState::new(
        dir,
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    if let Some(f) = settings {
        st.settings = Settings::load(f, None).unwrap();
    }
    Arc::new(st)
}

/// A1 and A5: a declared entry applies to a dataset created later, persistent or in
/// memory, without a restart.
#[tokio::test(flavor = "multi_thread")]
async fn declared_entries_apply_to_new_datasets() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({"datasets": {
            "slurp": {"assistant": {"enabled": true}},
            "scratch": {"memory": {"agentGraphs": ["urn:x-sparkles:agents/*"]}}
        }})
        .to_string(),
    )
    .unwrap();
    let st = state(&dir.path().join("data"), Some(&f));
    let app = crate::http::router(st.clone());
    let (s, v, _) = send(&app, req("GET", "/$/settings", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["unmatched"], json!(["scratch", "slurp"]));
    for (name, kind) in [("slurp", "tdb2"), ("scratch", "mem")] {
        let (s, v, _) = send(
            &app,
            req(
                "POST",
                &format!("/$/datasets?dbName={name}&dbType={kind}"),
                None,
            ),
        )
        .await;
        assert!(s.is_success(), "{s} {v}");
    }
    let (s, v, etag) = send(&app, req("GET", "/$/settings/slurp/assistant", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["effective"]["enabled"], true);
    assert_eq!(v["sources"]["enabled"], "declared");
    assert_eq!(v["runtime"], json!({}));
    assert_eq!(etag.as_deref(), v["etag"].as_str());
    let (_, v, _) = send(&app, req("GET", "/$/settings/scratch/memory", None)).await;
    assert_eq!(
        v["effective"]["agentGraphs"],
        json!(["urn:x-sparkles:agents/*"])
    );
    assert_eq!(v["sources"]["agentGraphs"], "declared");
    let ds = st.get("scratch").unwrap();
    assert!(crate::assist::memory_settings(&st, &ds).is_agent_graph("urn:x-sparkles:agents/a"));
    // the older route answers the effective object in its shape
    let (_, v, _) = send(&app, req("GET", "/$/assistant/slurp", None)).await;
    assert_eq!(v["enabled"], true);
    assert!(v["status"].is_object());
    let (_, v, _) = send(&app, req("GET", "/$/settings", None)).await;
    assert_eq!(v["unmatched"], json!([]));
    let (s, v, _) = send(&app, req("GET", "/$/settings/slurp", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["kinds"]["ingest"]["effective"], json!({"keepText": true}));
    let (s, v, _) = send(&app, req("GET", "/$/settings/slurp/other", None)).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown-kind"))
    );
}

/// A2: a runtime value outlives a restart and a reload, and clearing it brings back the
/// declared value.
#[tokio::test(flavor = "multi_thread")]
async fn runtime_values_persist_and_reset() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({"datasets": {"slurp": {"assistant": {"historyDays": 30}}}}).to_string(),
    )
    .unwrap();
    {
        let st = state(&data, Some(&f));
        st.create("slurp", DbType::Persistent).unwrap();
        let app = crate::http::router(st.clone());
        let (s, v, _) = send(
            &app,
            req(
                "PATCH",
                "/$/settings/slurp/assistant",
                Some(json!({"historyDays": 7})),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["runtime"], json!({"historyDays": 7}));
        // the runtime file holds only the change
        let ds = st.get("slurp").unwrap();
        let file: Value = serde_json::from_slice(
            &std::fs::read(ds.store.root().unwrap().join("assistant.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(file, json!({"historyDays": 7}));
    }
    // a restart
    let st = state(&data, Some(&f));
    let app = crate::http::router(st.clone());
    let (_, v, _) = send(&app, req("GET", "/$/settings/slurp/assistant", None)).await;
    assert_eq!(v["effective"]["historyDays"], 7);
    assert_eq!(v["sources"]["historyDays"], "runtime");
    // a reload with a changed file
    std::fs::write(
        &f,
        json!({"datasets": {"slurp": {"assistant": {"historyDays": 40, "explain": true}}}})
            .to_string(),
    )
    .unwrap();
    st.settings.reload(None).unwrap();
    let (_, v, _) = send(&app, req("GET", "/$/settings/slurp/assistant", None)).await;
    assert_eq!(v["effective"]["historyDays"], 7);
    assert_eq!(v["effective"]["explain"], true);
    let (s, v, _) = send(
        &app,
        req(
            "DELETE",
            "/$/settings/slurp/assistant?field=historyDays",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effective"]["historyDays"], 40);
    assert_eq!(v["sources"]["historyDays"], "declared");
    let ds = st.get("slurp").unwrap();
    assert!(!ds.store.root().unwrap().join("assistant.json").exists());
    assert_eq!(crate::assistant::settings(&st, &ds).history_days, Some(40));

    // A4 on a reload: the previous file stays, and the status says why
    std::fs::write(
        &f,
        json!({"datasets": {"slurp": {"assistant": {"send": "everything"}}}}).to_string(),
    )
    .unwrap();
    assert!(st.settings.reload(None).is_err());
    let (_, v, _) = send(&app, req("GET", "/$/settings/slurp/assistant", None)).await;
    assert_eq!(v["effective"]["historyDays"], 40);
    let (_, v, _) = send(&app, req("GET", "/$/settings", None)).await;
    assert!(v["error"].as_str().unwrap().contains("everything"), "{v}");
    assert!(v["readAt"].is_string());
}

/// A3, A7 and the semantics of `PUT`.
#[tokio::test(flavor = "multi_thread")]
async fn locks_preconditions_and_put() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({
            "defaults": {"assistant": {"send": "schema"}, "locked": ["assistant.send"]},
            "datasets": {"slurp": {"assistant": {"enabled": true, "budget": {"perRequest": 900}}}}
        })
        .to_string(),
    )
    .unwrap();
    let st = state(&dir.path().join("data"), Some(&f));
    st.create("slurp", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());
    let u = "/$/settings/slurp/assistant";
    let (s, v, _) = send(&app, req("PATCH", u, Some(json!({"send": "documents"})))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["send"]));
    // restating the locked value stores nothing for it
    let (s, v, _) = send(
        &app,
        req(
            "PUT",
            u,
            Some(json!({"enabled": true, "send": "schema", "ingest": true})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // the PUT made the effective object equal to the body: the declared budget is gone
    assert_eq!(
        v["runtime"],
        json!({"ingest": true, "budget": {"perRequest": null}})
    );
    assert_eq!(v["effective"]["budget"], json!({}));
    assert_eq!(v["sources"]["send"], "locked");
    // a PUT that leaves a locked field out keeps its value
    let (s, v, _) = send(&app, req("PUT", u, Some(json!({"enabled": true})))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["runtime"], json!({"budget": {"perRequest": null}}));
    // A7
    let (_, v, etag) = send(&app, req("GET", u, None)).await;
    let etag = etag.unwrap();
    assert_eq!(v["etag"], etag.as_str());
    let patch = |body: Value| {
        Request::builder()
            .method("PATCH")
            .uri(u)
            .header("content-type", "application/json")
            .header("if-match", etag.as_str())
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (s, v, new) = send(&app, patch(json!({"explain": true}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_ne!(new.as_deref(), Some(etag.as_str()));
    let (s, v, _) = send(&app, patch(json!({"optimize": true}))).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    assert_eq!(v["code"], "precondition-failed");
    // invalid values and keys are refused
    let (s, v, _) = send(&app, req("PATCH", u, Some(json!({"rowsForSummary": 0})))).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad-settings"))
    );
    let (s, _, _) = send(&app, req("PATCH", u, Some(json!({"sned": "rows"})))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v, _) = send(
        &app,
        req(
            "PATCH",
            u,
            Some(json!({"roles": {"draft": [{"provider": "p", "model": "m", "apiKey": "k"}]}})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("apiKey"));
    // a role that names no provider of the server
    let (s, _, _) = send(
        &app,
        req(
            "PATCH",
            u,
            Some(json!({"roles": {"draft": [{"provider": "p", "model": "m"}]}})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // DELETE clears the runtime layer
    let (s, v, _) = send(&app, req("DELETE", u, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["runtime"], json!({}));
    assert_eq!(v["effective"]["budget"]["perRequest"], 900);
    // the older PUT behaves as the new one and answers in its own shape
    let (s, v, _) = send(
        &app,
        req(
            "PUT",
            "/$/assistant/slurp",
            Some(json!({"enabled": true, "send": "rows"})),
        ),
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("locked-by-config"))
    );
    let (s, v, _) = send(
        &app,
        req(
            "PUT",
            "/$/assistant/slurp",
            Some(json!({"enabled": true, "explain": true, "status": {}})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["explain"], true);
    assert!(v["status"].is_object());
}

/// A lock added while a runtime value exists ignores the value and reports it.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_lock_overrides_a_runtime_value() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(&f, "{}").unwrap();
    let st = state(&dir.path().join("data"), Some(&f));
    st.create("slurp", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());
    let u = "/$/settings/slurp/assistant";
    let (s, _, _) = send(&app, req("PATCH", u, Some(json!({"send": "rows"})))).await;
    assert_eq!(s, StatusCode::OK);
    std::fs::write(
        &f,
        json!({"defaults": {"locked": ["assistant.send"]}}).to_string(),
    )
    .unwrap();
    st.settings.reload(None).unwrap();
    let (_, v, _) = send(&app, req("GET", u, None)).await;
    assert_eq!(v["effective"]["send"], "schema");
    assert_eq!(v["runtime"], json!({"send": "rows"}));
    assert_eq!(v["overridden"], json!(["send"]));
    assert_eq!(v["sources"]["send"], "locked");
    // another field can still change, and the ignored value stays
    let (s, v, _) = send(&app, req("PATCH", u, Some(json!({"ingest": true})))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["runtime"], json!({"send": "rows", "ingest": true}));
    let ds = st.get("slurp").unwrap();
    assert_eq!(
        crate::assistant::settings(&st, &ds).send,
        crate::assistant::Send::Schema
    );
}

/// A9 in the process: a dataset of `--loc` or `--mem` is declared, and the catalog API
/// does not delete it.
#[tokio::test(flavor = "multi_thread")]
async fn declared_datasets_are_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(&dir.path().join("data"), None);
    let loc = dir.path().join("demo");
    st.attach("demo", DbType::Persistent, Some(&loc)).unwrap();
    st.attach("m", DbType::Mem, None).unwrap();
    st.create("made", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());
    let (_, v, _) = send(&app, req("GET", "/$/datasets", None)).await;
    let declared: BTreeMap<String, bool> = v["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_string(),
                d["declared"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        declared,
        BTreeMap::from([
            ("demo".into(), true),
            ("m".into(), true),
            ("made".into(), false)
        ])
    );
    let before: Vec<_> = std::fs::read_dir(&loc)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    for name in ["demo", "m"] {
        let (s, v, _) = send(&app, req("DELETE", &format!("/$/datasets/{name}"), None)).await;
        assert_eq!(
            (s, v["code"].as_str()),
            (StatusCode::CONFLICT, Some("declared-dataset"))
        );
        assert!(st.get(name).is_some());
    }
    let after: Vec<_> = std::fs::read_dir(&loc)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
    let (s, _, _) = send(&app, req("DELETE", "/$/datasets/made", None)).await;
    assert_eq!(s, StatusCode::OK);
}

/// The `ingest` kind holds the settings members of `ingest.json`, and the profiles in
/// the same file stay as they are.
#[cfg(feature = "mcp")]
#[tokio::test(flavor = "multi_thread")]
async fn ingest_settings_share_their_file_with_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({"defaults": {"ingest": {"confirmTokens": 1000}}}).to_string(),
    )
    .unwrap();
    let st = state(&dir.path().join("data"), Some(&f));
    st.create("slurp", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());
    let (s, _, _) = send(
        &app,
        req(
            "PUT",
            "/$/ingest/slurp/profiles/notes",
            Some(json!({"language": "en"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // the older route patches: a member left out keeps its value
    let (s, v, _) = send(
        &app,
        req(
            "PUT",
            "/$/ingest/slurp/settings",
            Some(json!({"keepText": false})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({"dataset": "slurp", "keepText": false, "confirmTokens": 1000})
    );
    let ds = st.get("slurp").unwrap();
    let file: Value = serde_json::from_slice(
        &std::fs::read(ds.store.root().unwrap().join("ingest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        file,
        json!({"keepText": false, "profiles": {"notes": {"language": "en"}}})
    );
    let (_, v, _) = send(&app, req("GET", "/$/settings/slurp/ingest", None)).await;
    assert_eq!(v["runtime"], json!({"keepText": false}));
    assert_eq!(v["sources"]["confirmTokens"], "declared");
    let s = crate::mcp::memory::ingest::ingest_settings(&st, &ds);
    assert!(!s.keep_text);
    assert_eq!(s.confirm_tokens, Some(1000));
    assert!(s.profiles.contains_key("notes"));
    // null falls back to the declared value
    let (_, v, _) = send(
        &app,
        req(
            "PUT",
            "/$/ingest/slurp/settings",
            Some(json!({"confirmTokens": 5, "keepText": null})),
        ),
    )
    .await;
    assert_eq!(
        v,
        json!({"dataset": "slurp", "keepText": true, "confirmTokens": 5})
    );
    // clearing the kind keeps the profiles
    let (s, _, _) = send(&app, req("DELETE", "/$/settings/slurp/ingest", None)).await;
    assert_eq!(s, StatusCode::OK);
    let file: Value = serde_json::from_slice(
        &std::fs::read(ds.store.root().unwrap().join("ingest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(file, json!({"profiles": {"notes": {"language": "en"}}}));
}

/// The model configuration that SIGHUP reads again replaces the one requests start
/// with, and one that does not load leaves the previous in place.
#[tokio::test(flavor = "multi_thread")]
async fn models_reload() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("models.json");
    let write = |role_model: &str| {
        std::fs::write(
            &cfg,
            json!({"models": {
                "providers": {"local": {"kind": "openai", "endpoint": "http://127.0.0.1:9/v1"}},
                "roles": {"draft": [{"provider": "local", "model": role_model}]}
            }})
            .to_string(),
        )
        .unwrap();
    };
    write("a");
    let args = crate::models::ModelArgs {
        model_config: Some(cfg.clone()),
        model_secret: Vec::new(),
    };
    let mut st = AppState::new(
        &dir.path().join("data"),
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    st.set_models(args.load(Default::default()).unwrap());
    st.settings.set_models_file(Some(cfg.clone()));
    let st = Arc::new(st);
    let held = st.models().unwrap();
    write("b");
    reload(&st, &args);
    let now = st.models().unwrap();
    assert_eq!(now.pairs(crate::models::Role::Draft)[0].model, "b");
    // a request that started before keeps its configuration
    assert_eq!(held.pairs(crate::models::Role::Draft)[0].model, "a");
    std::fs::write(&cfg, "{").unwrap();
    reload(&st, &args);
    assert_eq!(
        st.models().unwrap().pairs(crate::models::Role::Draft)[0].model,
        "b"
    );
    let v = st.settings.status_json(&st);
    assert!(v["models"]["error"].is_string(), "{v}");
}
