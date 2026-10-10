//! The server-wide `models` kind and the runtime secrets of spec C19 §11, in the
//! process: the layers and locks, the routes, A13 to A15, and a restart.
//! `router_tests/auth/server_settings.rs` runs A11 and A12 with access control, and
//! `tests/cli_server_settings.rs` runs a server process with SIGHUP and its log.

use super::*;
use crate::models::mock::{self, MockModel};
use crate::state::DbType;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// A model configuration: `claude` (anthropic, key `anthropic`) and `local` (openai at
/// `local`), with `local/m` drafting.
fn declared_models(local: &str) -> Value {
    json!({"models": {
        "providers": {
            "claude": {
                "kind": "anthropic", "endpoint": "https://api.anthropic.com",
                "apiKey": {"secret": "anthropic"}, "budget": {"tokensPerDay": 1000}
            },
            "local": {"kind": "openai", "endpoint": local, "apiKey": {"secret": "gw"}}
        },
        "roles": {"draft": [{"provider": "local", "model": "m"}]}
    }})
}

struct Fixture {
    dir: tempfile::TempDir,
    st: Arc<AppState>,
    app: axum::Router,
}

/// A server state over `dir/data` with the model configuration `models` (none for
/// `Null`), the settings file `settings`, `--model-secret` flags, and the outbound
/// policy's `allow_private`.
fn open(
    dir: &std::path::Path,
    models: &Value,
    settings: Option<&Value>,
    secrets: &[String],
    allow_private: bool,
) -> (Arc<AppState>, axum::Router) {
    let mut st = AppState::new(
        &dir.join("data"),
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    st.outbound.allow_private = allow_private;
    let model_config = (!models.is_null()).then(|| {
        let f = dir.join("models.json");
        std::fs::write(&f, models.to_string()).unwrap();
        f
    });
    let settings_file = settings.map(|s| {
        let f = dir.join("settings.json");
        std::fs::write(&f, s.to_string()).unwrap();
        f
    });
    let args = crate::models::ModelArgs {
        model_config,
        model_secret: secrets.to_vec(),
    };
    server::start(&mut st, settings_file.as_deref(), &args).unwrap();
    let st = Arc::new(st);
    let app = crate::http::router(st.clone());
    (st, app)
}

fn fixture(models: Value, settings: Option<Value>, allow_private: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = open(dir.path(), &models, settings.as_ref(), &[], allow_private);
    Fixture { dir, st, app }
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let b = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let etag = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null), etag)
}

const MODELS_URI: &str = "/$/server/settings/models";

#[test]
fn locks_of_server_settings_are_checked() {
    let bad = |locked: Value| {
        Declared::parse(
            &json!({"server": {"locked": locked}}).to_string(),
            Providers::Unchecked,
        )
        .unwrap_err()
    };
    assert!(bad(json!(["models.provider"])).contains("no member"));
    assert!(bad(json!(["models.providers.claude.endpont"])).contains("no member"));
    assert!(bad(json!(["models.roles.drafts"])).contains("no role"));
    assert!(bad(json!(["models.roles.draft.0"])).contains("one field"));
    assert!(bad(json!(["models.routing.x"])).contains("routing"));
    assert!(bad(json!(["secrets.a.b"])).contains("secret"));
    assert!(bad(json!(["secrets.../x"])).contains("secret"));
    assert!(bad(json!(["models.providers.claude.tls.verify"])).contains("tls has no member"));
    assert!(bad(json!(["models.providers.claude.tls.caCert.file"])).contains("one field"));
    let d = Declared::parse(
        &json!({"server": {"locked": [
            "models.providers.claude", "models.providers.local.endpoint",
            "models.roles.draft", "models.routing", "secrets.anthropic"
        ]}})
        .to_string(),
        Providers::Unchecked,
    )
    .unwrap();
    assert_eq!(d.server_locked(&MODELS).len(), 4);
    assert!(d.secret_locked("anthropic"));
    assert!(!d.secret_locked("gw"));
    // against a model configuration: a provider it lacks, a secret nobody uses
    let cfg =
        crate::models::ModelsConfig::parse(&declared_models("http://127.0.0.1:9/v1").to_string())
            .unwrap();
    let d = Declared::parse(
        &json!({"server": {"locked": ["models.providers.other.endpoint", "secrets.nobody", "secrets.gw"]}})
            .to_string(),
        Providers::Unchecked,
    )
    .unwrap();
    let w = server::lock_warnings(&d, &cfg);
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("\"other\""), "{w:?}");
    assert!(w[1].contains("secrets.nobody"), "{w:?}");
}

/// The layers of §11.1: providers merge member by member, a role list is one field, a
/// runtime `null` removes a declared provider and `DELETE ?field=` brings it back.
#[tokio::test(flavor = "multi_thread")]
async fn models_layers_and_routes() {
    let f = fixture(declared_models("http://127.0.0.1:9/v1"), None, true);
    let (s, v, etag) = send(&f.app, "GET", MODELS_URI, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["scope"], "server");
    assert_eq!(v["kind"], "models");
    assert!(v.get("dataset").is_none());
    assert_eq!(v["sources"]["providers.claude.endpoint"], "declared");
    assert_eq!(v["status"]["valid"], true);
    assert_eq!(v["etag"], etag.as_str());
    // add a provider and a role list; change a budget
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({
            "providers": {
                "gw": {"kind": "openai", "endpoint": "http://127.0.0.1:10/v1"},
                "claude": {"budget": {"tokensPerDay": 5}}
            },
            "roles": {"draft": [{"provider": "gw", "model": "x"}]}
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["effective"]["providers"]["claude"]["endpoint"],
        "https://api.anthropic.com"
    );
    assert_eq!(
        v["effective"]["providers"]["claude"]["budget"]["tokensPerDay"],
        5
    );
    assert_eq!(
        v["sources"]["providers.claude.budget.tokensPerDay"],
        "runtime"
    );
    assert_eq!(v["sources"]["providers.claude.kind"], "declared");
    assert_eq!(
        v["effective"]["roles"]["draft"],
        json!([{"provider": "gw", "model": "x"}])
    );
    let m = f.st.models().unwrap();
    assert!(m.provider("gw").is_some());
    assert_eq!(m.pairs(crate::models::Role::Draft)[0].provider, "gw");
    let file = f.dir.path().join("data").join(server::MODELS_FILE);
    let stored: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(stored, v["runtime"]);
    // a request in flight keeps the configuration it started with
    let held = f.st.models().unwrap();
    // removing a declared provider stores null
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"local": null}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["runtime"]["providers"]["local"], Value::Null);
    assert!(
        v["runtime"]["providers"]
            .as_object()
            .unwrap()
            .contains_key("local")
    );
    assert!(v["effective"]["providers"].get("local").is_none());
    assert!(f.st.models().unwrap().provider("local").is_none());
    assert!(held.provider("local").is_some());
    // a role list that names a removed provider is refused
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"roles": {"draft": [{"provider": "local", "model": "m"}]}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "bad-settings");
    // DELETE ?field= restores the declared provider
    let (s, v, _) = send(
        &f.app,
        "DELETE",
        &format!("{MODELS_URI}?field=providers.local"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effective"]["providers"]["local"]["kind"], "openai");
    // a runtime-only provider's null removes it from the runtime layer
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"gw": null}, "roles": {"draft": null}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["runtime"]["providers"].get("gw").is_none());
    assert_eq!(v["effective"]["roles"]["draft"][0]["provider"], "local");
    // If-Match
    let (_, _, etag) = send(&f.app, "GET", MODELS_URI, None).await;
    let req = |tag: &str| {
        Request::builder()
            .method("PATCH")
            .uri(MODELS_URI)
            .header("content-type", "application/json")
            .header("if-match", tag)
            .body(Body::from(
                json!({"routing": {"exampleScore": 0.5}}).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        f.app.clone().oneshot(req(&etag)).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        f.app.clone().oneshot(req(&etag)).await.unwrap().status(),
        StatusCode::PRECONDITION_FAILED
    );
    // PUT makes the effective object equal to the body: leaving out a declared
    // provider removes it
    let mut body = declared_models("http://127.0.0.1:9/v1")["models"].clone();
    body["providers"].as_object_mut().unwrap().remove("claude");
    let (s, v, _) = send(&f.app, "PUT", MODELS_URI, Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["runtime"], json!({"providers": {"claude": null}}));
    // DELETE clears the runtime layer, and its file
    let (s, v, _) = send(&f.app, "DELETE", MODELS_URI, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["runtime"], json!({}));
    assert!(!file.exists());
    assert!(f.st.models().unwrap().provider("claude").is_some());
    // unknown kinds
    let (s, v, _) = send(&f.app, "GET", "/$/server/settings/assistant", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["code"], "unknown-kind");
    // the status lists the server-wide kinds
    let (_, v, _) = send(&f.app, "GET", "/$/settings", None).await;
    assert_eq!(v["serverKinds"], json!(["models"]));
}

/// The checks of the model configuration apply to the effective object.
#[tokio::test(flavor = "multi_thread")]
async fn credentials_and_bad_endpoints_are_refused() {
    let f = fixture(declared_models("http://127.0.0.1:9/v1"), None, true);
    for (patch, want) in [
        (
            json!({"providers": {"local": {"headers": {"Authorization": "Bearer sk-live-1"}}}}),
            "carries a key",
        ),
        (
            json!({"providers": {"local": {"endpoint": "http://u:sk-live-1@h.example/v1"}}}),
            "credentials",
        ),
        (
            json!({"providers": {"local": {"endpoint": "http://h.example/v1?key=sk-live-1"}}}),
            "query",
        ),
        (
            json!({"providers": {"local": {"endpoint": "http://h.example/v1#sk-live-1"}}}),
            "fragment",
        ),
        (
            json!({"providers": {"local": {"apiKey": "sk-live-1"}}}),
            "apiKey must be",
        ),
        (
            json!({"providers": {"local": {"kind": "other"}}}),
            "unknown variant",
        ),
        (json!({"providers": {"local": {"x": 1}}}), "unknown field"),
        (json!({"rolez": 1}), "unknown field"),
    ] {
        let (s, v, _) = send(&f.app, "PATCH", MODELS_URI, Some(patch.clone())).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{patch}: {v}");
        let e = v["error"].as_str().unwrap();
        assert!(e.contains(want), "{patch}: {e}");
        if want == "apiKey must be" || want == "carries a key" {
            assert!(!e.contains("sk-live-1"), "{e}");
        }
    }
    let (_, v, _) = send(&f.app, "GET", MODELS_URI, None).await;
    assert_eq!(v["runtime"], json!({}));
}

/// A13: a locked endpoint is a `409`, a budget is not; a whole provider locked fixes
/// every field and its presence.
#[tokio::test(flavor = "multi_thread")]
async fn a13_locked_fields() {
    let f = fixture(
        declared_models("http://127.0.0.1:9/v1"),
        Some(
            json!({"server": {"locked": ["models.providers.claude.endpoint", "models.providers.local", "models.routing"]}}),
        ),
        true,
    );
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"claude": {"endpoint": "https://evil.example"}}})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["providers.claude.endpoint"]));
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"claude": {"budget": {"tokensPerDay": 7}}}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["sources"]["providers.claude.endpoint"], "locked");
    assert_eq!(
        v["locked"],
        json!(["providers.claude.endpoint", "providers.local", "routing"])
    );
    // restating a locked value stores nothing
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"claude": {"endpoint": "https://api.anthropic.com"}}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["runtime"]["providers"]["claude"]
            .get("endpoint")
            .is_none()
    );
    for patch in [
        json!({"providers": {"local": {"concurrency": 9}}}),
        json!({"providers": {"local": null}}),
        json!({"providers": {"claude": null}}),
        json!({"routing": {"exampleScore": 0.1}}),
    ] {
        let (s, v, _) = send(&f.app, "PATCH", MODELS_URI, Some(patch.clone())).await;
        assert_eq!(s, StatusCode::CONFLICT, "{patch}: {v}");
    }
    // a runtime value stored before the lock is kept but ignored
    let f2 = {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(
            dir.path().join("data").join(server::MODELS_FILE),
            json!({"providers": {"claude": {"endpoint": "https://other.example"}}}).to_string(),
        )
        .unwrap();
        let (st, app) = open(
            dir.path(),
            &declared_models("http://127.0.0.1:9/v1"),
            Some(&json!({"server": {"locked": ["models.providers.claude.endpoint"]}})),
            &[],
            true,
        );
        (dir, st, app)
    };
    let (_, v, _) = send(&f2.2, "GET", MODELS_URI, None).await;
    assert_eq!(v["overridden"], json!(["providers.claude.endpoint"]));
    assert_eq!(
        v["effective"]["providers"]["claude"]["endpoint"],
        "https://api.anthropic.com"
    );
    assert_eq!(
        f2.1.models().unwrap().provider("claude").unwrap().endpoint,
        "https://api.anthropic.com"
    );
}

/// A lock on `tls.insecureSkipVerify` pins certificate checks on for a provider that
/// the declared configuration leaves verified (§11.6), while its CA certificate can still
/// change, and a CA certificate that is not a reference is a `400`.
#[tokio::test(flavor = "multi_thread")]
async fn locked_insecure_skip_verify() {
    let f = fixture(
        declared_models("http://127.0.0.1:9/v1"),
        Some(json!({"server": {"locked": ["models.providers.claude.tls.insecureSkipVerify"]}})),
        true,
    );
    for patch in [
        json!({"providers": {"claude": {"tls": {"insecureSkipVerify": true}}}}),
        json!({"providers": {"claude": {"tls": null}}}),
    ] {
        let (s, v, _) = send(&f.app, "PATCH", MODELS_URI, Some(patch.clone())).await;
        if patch["providers"]["claude"]["tls"].is_null() {
            // nothing declared and nothing stored: removing it changes nothing
            assert_eq!(s, StatusCode::OK, "{patch}: {v}");
            continue;
        }
        assert_eq!(s, StatusCode::CONFLICT, "{patch}: {v}");
        assert_eq!(v["code"], "locked-by-config");
        assert_eq!(
            v["fields"],
            json!(["providers.claude.tls.insecureSkipVerify"])
        );
    }
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"claude": {"tls": {"caCert": {"file": "/etc/ssl/internal-ca.pem"}}}}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["locked"],
        json!(["providers.claude.tls.insecureSkipVerify"]),
        "{v}"
    );
    assert!(
        !f.st
            .models()
            .unwrap()
            .provider("claude")
            .unwrap()
            .insecure()
    );
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"claude": {"tls": {"caCert": "-----BEGIN CERTIFICATE-----"}}}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("never written"),
        "{v}"
    );
}

/// `overrides` of the `models` kind: a changed budget, a removed provider and a role
/// list, each with the declared value it replaces, and a reset that brings the declared
/// value back. A locked field's ignored runtime value is not an override.
#[tokio::test(flavor = "multi_thread")]
async fn overrides_of_the_models_kind() {
    let f = fixture(declared_models("http://127.0.0.1:9/v1"), None, true);
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({
            "providers": {
                "claude": {"budget": {"tokensPerDay": 7}, "concurrency": 2},
                "local": null
            },
            "roles": {"draft": [{"provider": "claude", "model": "c"}]}
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["scope"], "server");
    let local =
        json!({"kind": "openai", "endpoint": "http://127.0.0.1:9/v1", "apiKey": {"secret": "gw"}});
    assert_eq!(
        v["overrides"],
        json!([
            {"path": "providers.claude.budget.tokensPerDay", "declared": 1000, "runtime": 7},
            {"path": "providers.local", "declared": local, "runtime": null},
            {"path": "roles.draft", "declared": [{"provider": "local", "model": "m"}],
             "runtime": [{"provider": "claude", "model": "c"}]},
        ])
    );
    assert_eq!(v["sources"]["providers.claude.concurrency"], "runtime");
    let (s, v, _) = send(
        &f.app,
        "DELETE",
        &format!("{MODELS_URI}?field=providers.claude.budget.tokensPerDay"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["sources"]["providers.claude.budget.tokensPerDay"],
        "declared"
    );
    assert_eq!(v["overrides"].as_array().unwrap().len(), 2, "{v}");

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::write(
        dir.path().join("data").join(server::MODELS_FILE),
        json!({"roles": {"draft": [{"provider": "claude", "model": "c"}]}}).to_string(),
    )
    .unwrap();
    let (_st, app) = open(
        dir.path(),
        &declared_models("http://127.0.0.1:9/v1"),
        Some(&json!({"server": {"locked": ["models.roles.draft"]}})),
        &[],
        true,
    );
    let (_, v, _) = send(&app, "GET", MODELS_URI, None).await;
    assert_eq!(v["overridden"], json!(["roles.draft"]), "{v}");
    assert_eq!(v["overrides"], json!([]));
}

/// A dataset's role list that names a provider removed at runtime is reported in the
/// dataset's status.
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_provider_shows_in_the_dataset_status() {
    let f = fixture(declared_models("http://127.0.0.1:9/v1"), None, true);
    f.st.attach("org", DbType::Mem, None).unwrap();
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        "/$/settings/org/assistant",
        Some(json!({"enabled": true, "roles": {"draft": [{"provider": "claude", "model": "c"}]}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"claude": null}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v, _) = send(&f.app, "GET", "/$/settings/org/assistant", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"]["valid"], false, "{v}");
    assert!(
        v["status"]["error"].as_str().unwrap().contains("claude"),
        "{v}"
    );
    let (s, v, _) = send(&f.app, "GET", "/$/assistant/org", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Without `--model-config`, a runtime provider configures models.
#[tokio::test(flavor = "multi_thread")]
async fn runtime_providers_without_a_model_configuration() {
    let f = fixture(Value::Null, None, true);
    let (_, v, _) = send(&f.app, "GET", "/$/models", None).await;
    assert_eq!(v["configured"], false);
    let (s, v, _) = send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"gw": {"kind": "openai", "endpoint": "http://127.0.0.1:9/v1"}}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v, _) = send(&f.app, "GET", "/$/models", None).await;
    assert_eq!(v["configured"], true);
    assert_eq!(v["providers"][0]["name"], "gw");
    let (s, _, _) = send(&f.app, "DELETE", MODELS_URI, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(f.st.models().is_none());
}

fn key_mock() -> MockModel {
    MockModel::start(|_, _| (200, mock::openai(r#"{"answer": "ok"}"#)))
}

fn bearer(m: &MockModel) -> Option<String> {
    m.requests()
        .last()
        .and_then(|r| r.header("authorization").map(str::to_string))
}

/// A14: a runtime key overrides a declared `file:` source, `DELETE` brings the file
/// back, and a locked secret refuses a `PUT`. No answer carries a value.
#[tokio::test(flavor = "multi_thread")]
async fn a14_runtime_secrets() {
    let m = key_mock();
    let dir = tempfile::tempdir().unwrap();
    let declared_key = dir.path().join("gw.key");
    std::fs::write(&declared_key, "declared-key-123\n").unwrap();
    let secrets = vec![format!("gw=file:{}", declared_key.display())];
    let models = declared_models(&format!("{}/v1", m.url()));
    let (st, app) = open(dir.path(), &models, None, &secrets, true);
    let test = || {
        send(
            &app,
            "POST",
            "/$/models/local/test",
            Some(json!({"model": "m"})),
        )
    };
    let (s, v, _) = test().await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(bearer(&m).as_deref(), Some("Bearer declared-key-123"));
    // a runtime value
    let (s, v, _) = send(
        &app,
        "PUT",
        "/$/server/secrets/gw",
        Some(json!({"value": "runtime-key-456"})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    test().await;
    assert_eq!(bearer(&m).as_deref(), Some("Bearer runtime-key-456"));
    let (s, v, _) = send(&app, "GET", "/$/server/secrets", None).await;
    assert_eq!(s, StatusCode::OK);
    let text = v.to_string();
    assert!(
        !text.contains("runtime-key-456") && !text.contains("declared-key-123"),
        "{text}"
    );
    let gw = v["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "gw")
        .unwrap();
    assert_eq!(gw["source"], "runtime");
    assert_eq!(gw["declared"], true);
    assert_eq!(gw["locked"], false);
    assert!(gw["setAt"].is_string());
    assert_eq!(gw["providers"], json!(["local"]));
    let anth = v["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "anthropic")
        .unwrap();
    assert_eq!(anth["source"], "missing");
    assert_eq!(anth["providers"], json!(["claude"]));
    // GET /$/models reports the source and a missing key
    let (_, v, _) = send(&app, "GET", "/$/models", None).await;
    let text = v.to_string();
    assert!(!text.contains("runtime-key-456"), "{text}");
    let prov = |n: &str| {
        v["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(
        prov("local")["apiKey"],
        json!({"secret": "gw", "source": "runtime"})
    );
    assert_eq!(prov("claude")["apiKey"]["source"], "missing");
    assert_eq!(prov("claude")["status"], "secret-missing");
    // the files
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sdir = dir.path().join("data").join("secrets");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&sdir), 0o700);
        assert_eq!(mode(&sdir.join("gw")), 0o600);
    }
    // a restart keeps the runtime value
    drop(app);
    drop(st);
    let (st, app) = open(dir.path(), &models, None, &secrets, true);
    send(
        &app,
        "POST",
        "/$/models/local/test",
        Some(json!({"model": "m"})),
    )
    .await;
    assert_eq!(bearer(&m).as_deref(), Some("Bearer runtime-key-456"));
    // DELETE brings back the declared file
    let (s, _, _) = send(&app, "DELETE", "/$/server/secrets/gw", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    send(
        &app,
        "POST",
        "/$/models/local/test",
        Some(json!({"model": "m"})),
    )
    .await;
    assert_eq!(bearer(&m).as_deref(), Some("Bearer declared-key-123"));
    let (s, _, _) = send(&app, "DELETE", "/$/server/secrets/gw", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // names and bodies
    let (s, v, _) = send(
        &app,
        "PUT",
        "/$/server/secrets/..%2Fx",
        Some(json!({"value": "k"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "bad-secret-name");
    let (s, v, _) = send(
        &app,
        "PUT",
        "/$/server/secrets/gw",
        Some(json!({"valu": "sk-oops-789"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["code"], "bad-secret");
    assert!(!v.to_string().contains("sk-oops-789"));
    drop((st, app));
    // a locked secret: its declared source applies and a PUT is a 409
    let settings = json!({"server": {"locked": ["secrets.gw"]}});
    let (st, app) = open(dir.path(), &models, Some(&settings), &secrets, true);
    let (s, v, _) = send(
        &app,
        "PUT",
        "/$/server/secrets/gw",
        Some(json!({"value": "runtime-key-456"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["secrets.gw"]));
    // a value stored before the lock is ignored
    std::fs::write(dir.path().join("data/secrets/gw"), "old-runtime").unwrap();
    server::apply(&st).unwrap();
    send(
        &app,
        "POST",
        "/$/models/local/test",
        Some(json!({"model": "m"})),
    )
    .await;
    assert_eq!(bearer(&m).as_deref(), Some("Bearer declared-key-123"));
    let (_, v, _) = send(&app, "GET", "/$/server/secrets", None).await;
    let gw = v["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "gw")
        .unwrap();
    assert_eq!(gw["source"], "declared");
    assert_eq!(gw["locked"], true);
    assert_eq!(gw["overridden"], true);
}

/// A15: a runtime provider on a private address is refused at request time unless the
/// outbound policy allows private addresses.
#[tokio::test(flavor = "multi_thread")]
async fn a15_private_addresses_need_the_outbound_flag() {
    let m = key_mock();
    let provider =
        json!({"providers": {"near": {"kind": "openai", "endpoint": format!("{}/v1", m.url())}}});
    for allow in [false, true] {
        let f = fixture(Value::Null, None, allow);
        let (s, v, _) = send(&f.app, "PATCH", MODELS_URI, Some(provider.clone())).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let before = m.requests().len();
        let (s, v, _) = send(
            &f.app,
            "POST",
            "/$/models/near/test",
            Some(json!({"model": "m"})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        if allow {
            assert_eq!(v["ok"], true, "{v}");
            assert_eq!(m.requests().len(), before + 1);
        } else {
            assert_eq!(v["ok"], false, "{v}");
            assert_eq!(v["error"]["code"], "outbound-refused", "{v}");
            assert_eq!(m.requests().len(), before);
        }
    }
}

/// A restart reads the runtime layer back, and a change of the declared configuration
/// that makes it invalid keeps the server up with the declared configuration.
#[tokio::test(flavor = "multi_thread")]
async fn restarts_keep_the_runtime_layer() {
    let dir = tempfile::tempdir().unwrap();
    let models = declared_models("http://127.0.0.1:9/v1");
    let (st, app) = open(dir.path(), &models, None, &[], true);
    let (s, v, _) = send(
        &app,
        "PATCH",
        MODELS_URI,
        Some(json!({"roles": {"summarize": [{"provider": "claude", "model": "c"}]}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    drop((st, app));
    let (st, app) = open(dir.path(), &models, None, &[], true);
    assert_eq!(
        st.models()
            .unwrap()
            .pairs(crate::models::Role::Summarize)
            .len(),
        1
    );
    drop(app);
    // the declared configuration drops `claude`, which the runtime role list names
    let mut fewer = models.clone();
    fewer["models"]["providers"]
        .as_object_mut()
        .unwrap()
        .remove("claude");
    drop(st);
    let (st, app) = open(dir.path(), &fewer, None, &[], true);
    let m = st.models().unwrap();
    assert!(m.pairs(crate::models::Role::Summarize).is_empty());
    let (_, v, _) = send(&app, "GET", MODELS_URI, None).await;
    assert_eq!(v["status"]["valid"], false, "{v}");
    let (_, v, _) = send(&app, "GET", "/$/settings", None).await;
    assert!(
        v["models"]["error"].as_str().unwrap().contains("claude"),
        "{v}"
    );
    // a reload with the full configuration applies the runtime layer again
    std::fs::write(dir.path().join("models.json"), models.to_string()).unwrap();
    let args = crate::models::ModelArgs {
        model_config: Some(dir.path().join("models.json")),
        model_secret: Vec::new(),
    };
    reload(&st, &args);
    assert_eq!(
        st.models()
            .unwrap()
            .pairs(crate::models::Role::Summarize)
            .len(),
        1
    );
    let (_, v, _) = send(&app, "GET", "/$/settings", None).await;
    assert!(v["models"]["error"].is_null(), "{v}");
}

/// The tokens a provider counted today survive a change of the configuration.
#[tokio::test(flavor = "multi_thread")]
async fn budgets_survive_a_change() {
    let m = key_mock();
    let f = fixture(Value::Null, None, true);
    let near = json!({"providers": {"near": {"kind": "openai", "endpoint": format!("{}/v1", m.url()), "budget": {"tokensPerDay": 1000}, "models": {"m": {"contextTokens": 4096}}}}});
    send(&f.app, "PATCH", MODELS_URI, Some(near)).await;
    send(
        &f.app,
        "POST",
        "/$/models/near/test",
        Some(json!({"model": "m"})),
    )
    .await;
    send(
        &f.app,
        "PATCH",
        MODELS_URI,
        Some(json!({"providers": {"near": {"concurrency": 2}}})),
    )
    .await;
    let (_, v, _) = send(&f.app, "GET", "/$/models", None).await;
    assert_eq!(v["providers"][0]["budget"]["usedToday"], 15, "{v}");
    assert_eq!(
        v["providers"][0]["models"][0]["status"]["state"], "ok",
        "{v}"
    );
}

/// A read-only server refuses every settings write, dataset-wide and server-wide, the
/// legacy routes included, as it refuses other admin writes, and still answers reads.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_server_refuses_settings_writes() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        &dir.path().join("data"),
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    st.read_only = true;
    server::start(&mut st, None, &Default::default()).unwrap();
    let st = Arc::new(st);
    st.attach("org", DbType::Mem, None).unwrap();
    let app = crate::http::router(st.clone());
    let body = Some(json!({}));
    for (method, uri, body) in [
        ("PATCH", "/$/settings/org/assistant", body.clone()),
        ("PUT", "/$/settings/org/memory", body.clone()),
        ("DELETE", "/$/settings/org/ingest", None),
        ("PUT", "/$/assistant/org", body.clone()),
        ("PUT", "/$/memory/org", body.clone()),
        (
            "PUT",
            "/$/ingest/org/settings",
            Some(json!({"keepText": false})),
        ),
        ("PATCH", MODELS_URI, body.clone()),
        ("PUT", MODELS_URI, body.clone()),
        ("DELETE", MODELS_URI, None),
        ("PUT", "/$/server/secrets/gw", Some(json!({"value": "k"}))),
        ("DELETE", "/$/server/secrets/gw", None),
    ] {
        let (s, v, _) = send(&app, method, uri, body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {uri}: {v}");
        assert_eq!(v["error"], "server is read-only", "{method} {uri}: {v}");
    }
    for uri in ["/$/settings/org/assistant", MODELS_URI, "/$/server/secrets"] {
        let (s, v, _) = send(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::OK, "{uri}: {v}");
    }
    assert!(!dir.path().join("data/secrets").exists());
}
