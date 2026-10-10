//! Declared prefixes (spec C20): the `prefixes` kind's layers, removals and locks,
//! `/{ds}/prefixes` on its runtime layer, the prefixes of loaded data, and the warnings
//! for prefixes that shadow well-known ones.

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

const KCLJ: &str = "https://kclj.io/sparkles/";
const NOTES: &str = "https://kclj.io/sparkles/memory/notes/";

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
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

fn turtle(uri: &str, text: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "text/turtle")
        .body(Body::from(text.to_string()))
        .unwrap()
}

fn state(dir: &std::path::Path, settings: &std::path::Path) -> Arc<AppState> {
    let mut st = AppState::new(
        dir,
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    st.settings = Settings::load(settings, None).unwrap();
    Arc::new(st)
}

fn settings_file(dir: &std::path::Path, v: Value) -> std::path::PathBuf {
    let f = dir.join("settings.json");
    std::fs::write(&f, v.to_string()).unwrap();
    f
}

fn the_file() -> Value {
    json!({
        "defaults": {"prefixes": {"kclj": KCLJ, "memory": "https://kclj.io/sparkles/memory/"}},
        "datasets": {"slurp": {
            "prefixes": {"notes": NOTES, "locked": "https://kclj.io/locked/"},
            "locked": ["prefixes.locked", "prefixes.nobody"]
        }}
    })
}

/// A1, A2 and A5: declared prefixes apply to a new dataset, a removal persists across a
/// restart and a reset brings the declared value back, and `GET /{ds}/prefixes` answers
/// the stored prefixes only.
#[tokio::test(flavor = "multi_thread")]
async fn declared_prefixes_layer_and_removals_persist() {
    let dir = tempfile::tempdir().unwrap();
    let f = settings_file(dir.path(), the_file());
    let data = dir.path().join("data");
    {
        let st = state(&data, &f);
        let app = crate::http::router(st.clone());
        let (s, _) = send(
            &app,
            req("POST", "/$/datasets?dbName=slurp&dbType=tdb2", None),
        )
        .await;
        assert!(s.is_success());
        let (s, v) = send(&app, req("GET", "/$/prefixes/slurp", None)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["prefixes"]["kclj"], KCLJ);
        assert_eq!(v["prefixes"]["notes"], NOTES);
        assert_eq!(
            v["prefixes"]["rdf"],
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
        );
        assert_eq!(v["prefixes"]["mem"], "urn:x-sparkles:mem:");
        let (s, v) = send(&app, req("GET", "/$/settings/slurp/prefixes", None)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["sources"]["kclj"], "declared");
        assert_eq!(v["sources"]["rdf"], "default");
        assert_eq!(v["sources"]["locked"], "locked");
        assert_eq!(v["locked"], json!(["locked", "nobody"]));
        assert_eq!(v["warnings"], json!([]));
        assert_eq!(v["status"]["valid"], true);
        // a runtime value over a declared one, and a removal of a declared one
        let (s, v) = send(
            &app,
            req(
                "PATCH",
                "/$/settings/slurp/prefixes",
                Some(json!({"kclj": "https://kclj.io/x/", "notes": null, "ex": "http://ex.org/"})),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["effective"]["kclj"], "https://kclj.io/x/");
        assert!(v["effective"].get("notes").is_none());
        assert_eq!(v["runtime"]["notes"], Value::Null);
        assert_eq!(
            v["overrides"],
            json!([
                {"path": "kclj", "declared": KCLJ, "runtime": "https://kclj.io/x/"},
                {"path": "notes", "declared": NOTES, "runtime": null}
            ])
        );
        // the Fuseki route answers the stored bindings only
        let (s, v) = send(&app, req("GET", "/slurp/prefixes", None)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            v["prefixes"],
            json!({"ex": "http://ex.org/", "kclj": "https://kclj.io/x/"})
        );
        let (s, _) = send(&app, req("GET", "/slurp/prefixes?prefix=notes", None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    // a restart keeps the removal and the runtime value
    let st = state(&data, &f);
    let app = crate::http::router(st.clone());
    let (_, v) = send(&app, req("GET", "/$/prefixes/slurp", None)).await;
    assert!(v["prefixes"].get("notes").is_none(), "{v}");
    assert_eq!(v["prefixes"]["kclj"], "https://kclj.io/x/");
    // resets bring the declared values back
    for f in ["notes", "kclj"] {
        let (s, _) = send(
            &app,
            req(
                "DELETE",
                &format!("/$/settings/slurp/prefixes?field={f}"),
                None,
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
    let (_, v) = send(&app, req("GET", "/$/prefixes/slurp", None)).await;
    assert_eq!(v["prefixes"]["notes"], NOTES);
    assert_eq!(v["prefixes"]["kclj"], KCLJ);
    let ds = st.get("slurp").unwrap();
    assert!(ds.store.removed_prefixes().is_empty());
}

/// A3: a locked prefix refuses runtime writes through both routes, and a lock on a name
/// nothing declares keeps it unbound.
#[tokio::test(flavor = "multi_thread")]
async fn locked_prefixes_refuse_writes() {
    let dir = tempfile::tempdir().unwrap();
    let f = settings_file(dir.path(), the_file());
    let st = state(&dir.path().join("data"), &f);
    let app = crate::http::router(st.clone());
    send(
        &app,
        req("POST", "/$/datasets?dbName=slurp&dbType=mem", None),
    )
    .await;
    let locked = |v: &Value| v["code"] == "locked-by-config";
    for (method, uri) in [
        ("POST", "/slurp/prefixes?prefix=locked&uri=http://other/"),
        ("PUT", "/slurp/prefixes?prefix=nobody&uri=http://other/"),
        ("DELETE", "/slurp/prefixes?prefix=locked"),
    ] {
        let (s, v) = send(&app, req(method, uri, None)).await;
        assert_eq!(s, StatusCode::CONFLICT, "{method} {uri}: {v}");
        assert!(locked(&v), "{v}");
    }
    let (s, v) = send(
        &app,
        req(
            "PATCH",
            "/$/settings/slurp/prefixes",
            Some(json!({"locked": null})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["fields"], json!(["locked"]));
    // restating the locked value stores nothing
    let (s, _) = send(
        &app,
        req(
            "POST",
            "/slurp/prefixes?prefix=locked&uri=https://kclj.io/locked/",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(st.get("slurp").unwrap().store.prefixes().is_empty());
    // a DELETE of a declared name stores a removal, then answers 404
    let (s, _) = send(&app, req("DELETE", "/slurp/prefixes?prefix=kclj", None)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = send(&app, req("DELETE", "/slurp/prefixes?prefix=kclj", None)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // a DELETE of a well-known name that is not stored is a 404, as before
    let (s, _) = send(&app, req("DELETE", "/slurp/prefixes?prefix=rdf", None)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // a POST clears the removal
    let (s, _) = send(
        &app,
        req(
            "POST",
            "/slurp/prefixes?prefix=kclj&uri=https://kclj.io/y/",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = send(&app, req("GET", "/$/settings/slurp/prefixes", None)).await;
    assert_eq!(v["runtime"], json!({"kclj": "https://kclj.io/y/"}));
    // a whole-kind lock fixes every prefix
    let f = settings_file(
        dir.path(),
        json!({"defaults": {"prefixes": {"kclj": KCLJ}, "locked": ["prefixes"]}}),
    );
    let st = state(&dir.path().join("data2"), &f);
    let app = crate::http::router(st.clone());
    send(
        &app,
        req("POST", "/$/datasets?dbName=other&dbType=mem", None),
    )
    .await;
    let (s, v) = send(
        &app,
        req("POST", "/other/prefixes?prefix=ex&uri=http://ex.org/", None),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, v) = send(&app, req("GET", "/$/settings/other/prefixes", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["locked"], json!([""]));
    assert_eq!(v["sources"]["kclj"], "locked");
}

/// A4: loaded data never replaces a declared or locked binding, nor a removed name.
#[tokio::test(flavor = "multi_thread")]
async fn loaded_data_keeps_declared_prefixes() {
    let dir = tempfile::tempdir().unwrap();
    let f = settings_file(dir.path(), the_file());
    let st = state(&dir.path().join("data"), &f);
    let app = crate::http::router(st.clone());
    send(
        &app,
        req("POST", "/$/datasets?dbName=slurp&dbType=tdb2", None),
    )
    .await;
    let (s, _) = send(&app, req("DELETE", "/slurp/prefixes?prefix=memory", None)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = send(
        &app,
        turtle(
            "/slurp/data?default",
            "@prefix kclj: <http://other/> .\n@prefix nobody: <http://nobody/> .\n\
             @prefix memory: <http://other/m/> .\n@prefix ex: <http://ex.org/> .\n\
             ex:a kclj:b ex:c .\n",
        ),
    )
    .await;
    assert!(s.is_success(), "{s} {v}");
    let ds = st.get("slurp").unwrap();
    assert_eq!(
        ds.store.prefixes().keys().collect::<Vec<_>>(),
        ["ex"],
        "declared, locked and removed names are not bound by data"
    );
    let (_, v) = send(&app, req("GET", "/$/prefixes/slurp", None)).await;
    assert_eq!(v["prefixes"]["kclj"], KCLJ);
    assert!(v["prefixes"].get("memory").is_none());
    // results are written with the declared and runtime prefixes
    let r = Request::builder()
        .method("GET")
        .uri("/slurp/data?default")
        .header("accept", "text/turtle")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(r).await.unwrap();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&b);
    assert!(text.contains("kclj:"), "{text}");
    assert!(!text.contains("@prefix rdfs:"), "{text}");
}

/// A6: a prefix that shadows a well-known one is allowed with a warning, and bad names
/// and relative IRIs fail the file.
#[tokio::test(flavor = "multi_thread")]
async fn shadowing_warns_and_bad_prefixes_fail() {
    let d = Declared::parse(
        &json!({"defaults": {"prefixes": {"mem": "https://example.org/mem#"}}}).to_string(),
        Providers::Unchecked,
    )
    .unwrap();
    let w = prefixes::file_warnings(&d);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("shadows the well-known mem:"), "{w:?}");
    for bad in [
        json!({"defaults": {"prefixes": {"bad name": KCLJ}}}),
        json!({"defaults": {"prefixes": {"kclj": "relative/iri"}}}),
        json!({"defaults": {"prefixes": {"kclj": 5}}}),
        json!({"defaults": {"locked": ["prefixes.bad name"]}}),
        json!({"defaults": {"locked": ["prefixes.a.b"]}}),
    ] {
        assert!(
            Declared::parse(&bad.to_string(), Providers::Unchecked).is_err(),
            "{bad}"
        );
    }
    // a prefix named like a forbidden member is fine
    Declared::parse(
        &json!({"defaults": {"prefixes": {"endpoint": KCLJ}}}).to_string(),
        Providers::Unchecked,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let f = settings_file(
        dir.path(),
        json!({"defaults": {"prefixes": {"mem": "https://example.org/mem#"}}}),
    );
    let st = state(&dir.path().join("data"), &f);
    let app = crate::http::router(st.clone());
    send(&app, req("POST", "/$/datasets?dbName=ds&dbType=mem", None)).await;
    let (_, v) = send(&app, req("GET", "/$/settings/ds/prefixes", None)).await;
    assert_eq!(v["warnings"][0]["prefix"], "mem");
    assert_eq!(v["warnings"][0]["wellKnown"], "urn:x-sparkles:mem:");
    // a write with a bad name or IRI is a 400
    for body in [json!({"bad name": KCLJ}), json!({"x": "rel"})] {
        let (s, v) = send(
            &app,
            req("PATCH", "/$/settings/ds/prefixes", Some(body.clone())),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}: {v}");
    }
}
