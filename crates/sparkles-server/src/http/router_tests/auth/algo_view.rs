//! View-only algorithm runs (U10 §3.2) under restricted views: a caller limited to one
//! named graph gets results computed over that graph only, never reads a result kept for
//! a wider view, and follows and cancels its own background run.

use super::*;
use serde_json::json;

/// The dataset `av`, read as the union of its named graphs.
const DATA: &str = r#"@prefix ex: <http://ex/> .
<http://ex/ga> { ex:a ex:p ex:b . ex:b ex:p ex:c . }
<http://ex/gb> { ex:c ex:p ex:a . ex:d ex:p ex:e . ex:e ex:p ex:a . }
"#;

fn users() -> String {
    users_with("http://ex/ga", true)
}

/// The users, with the graph `aga` reads and whether `aga2` may read `av` at all.
fn users_with(aga_graph: &str, aga2: bool) -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let aga2_ds = if aga2 { "av" } else { "elsewhere" };
    format!(
        r#"
[[users]]
name = "afull"
password = "{afull}"
datasets = {{ av = "read" }}

[[users]]
name = "aadmin"
password = "{aadmin}"
datasets = {{ av = "admin" }}

[[users]]
name = "amain"
password = "{amain}"
[[users.grants]]
dataset = "av"
level = "read"
branches = ["main"]

[[users]]
name = "adev"
password = "{adev}"
[[users.grants]]
dataset = "av"
level = "read"
branches = ["dev"]

[[users]]
name = "aga"
password = "{aga}"
[[users.grants]]
dataset = "av"
level = "read"
graphs = ["{aga_graph}"]

[[users]]
name = "aga2"
password = "{aga2}"
[[users.grants]]
dataset = "{aga2_ds}"
level = "read"
graphs = ["http://ex/ga"]
"#,
        afull = h("afull-pw"),
        aga = h("aga-pw"),
        aga2 = h("aga2-pw"),
        aadmin = h("aadmin-pw"),
        adev = h("adev-pw"),
        amain = h("amain-pw"),
    )
}

fn server() -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture {
        extra: users(),
        ..Default::default()
    };
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let opts = StoreOptions {
        union_default_graph: true,
        ..Default::default()
    };
    let mut st = AppState::new(dir.path(), opts, Duration::from_secs(30)).unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let st = Arc::new(st);
    let ds = st.attach("av", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    st.set_phase(crate::obs::Phase::Ready);
    let app = router(st.clone());
    (dir, st, app)
}

async fn run(app: &Router, user: &str, body: &J) -> R {
    call(
        app,
        "POST",
        "/$/algo/av/run",
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

fn rows(r: &R) -> Vec<(String, String)> {
    r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["node"]["value"].as_str().unwrap().to_string(),
                b["communitySize"]["value"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_restricted_view_never_reads_a_wider_kept_result() {
    let (_dir, _st, app) = server();
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a", "http://ex/d"],
        "keep": true,
    });
    let full = run(&app, "afull", &body).await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text());
    assert_eq!(
        rows(&full),
        [
            ("http://ex/a".into(), "5".into()),
            ("http://ex/d".into(), "5".into())
        ]
    );
    // the caller limited to ga: its own run, over a, b and c only, and nothing for d
    let ga = run(&app, "aga", &body).await;
    assert_eq!(ga.status, StatusCode::OK, "{}", ga.text());
    assert_eq!(ga.json()["run"]["cached"], false, "{}", ga.text());
    assert_eq!(rows(&ga), [("http://ex/a".into(), "3".into())]);
    // another caller with the same view reads that result, and the full one still its own
    let ga2 = run(&app, "aga2", &body).await;
    assert_eq!(ga2.json()["run"]["cached"], true);
    assert_eq!(rows(&ga2), rows(&ga));
    let again = run(&app, "afull", &body).await;
    assert_eq!(again.json()["run"]["cached"], true);
    assert_eq!(rows(&again), rows(&full));
}

/// The pause list is keyed by dataset name, and both background-run tests use "av", so
/// they take turns: one test's release would otherwise let the other's run finish early.
static AV_PAUSE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn a_restricted_caller_follows_and_cancels_its_own_background_run() {
    let _paused = AV_PAUSE.lock().await;
    let (_dir, st, app) = server();
    crate::http::algo::view::PAUSED.lock().push("av".into());
    let body = json!({
        "algorithm": "pageRank",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
        "async": true,
    });
    let r = run(&app, "aga", &body).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let uri = format!("/$/tasks/{id}");
    // the starter sees its task although its view is restricted
    assert_eq!(
        get_as(&app, &uri, Some(&b("aga"))).await.status,
        StatusCode::OK
    );
    // a caller with the same view joins it and may follow it, but not cancel it
    let j = run(&app, "aga2", &body).await;
    assert_eq!(j.json()["id"], id.as_str());
    assert_eq!(j.json()["joined"], true);
    assert_eq!(
        get_as(&app, &uri, Some(&b("aga2"))).await.status,
        StatusCode::OK
    );
    let r = call(&app, "DELETE", &uri, &[("authorization", &b("aga2"))], "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // the full reader runs its own, and does not see the restricted caller's task
    let f = run(&app, "afull", &body).await;
    assert_eq!(f.status, StatusCode::ACCEPTED, "{}", f.text());
    assert_ne!(f.json()["id"], id.as_str());
    let fid = f.json()["id"].as_str().unwrap().to_string();
    // the starter cancels
    let r = call(&app, "DELETE", &uri, &[("authorization", &b("aga"))], "").await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    crate::http::algo::view::PAUSED.lock().retain(|d| d != "av");
    let done = crate::http::router_tests::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "cancelled", "{:?}", done.message);
    let done = crate::http::router_tests::tasks::wait_done(&st, &fid).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
}

#[tokio::test]
async fn followers_lose_the_task_when_their_access_changes() {
    let _paused = AV_PAUSE.lock().await;
    let (dir, st, app) = server();
    crate::http::algo::view::PAUSED.lock().push("av".into());
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
        "async": true,
    });
    let r = run(&app, "aga", &body).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let uri = format!("/$/tasks/{id}");
    let j = run(&app, "aga2", &body).await;
    assert_eq!(j.json()["joined"], true, "{}", j.text());
    assert_eq!(
        get_as(&app, &uri, Some(&b("aga"))).await.status,
        StatusCode::OK
    );
    // aga now reads another graph, and aga2 nothing
    let f = Fixture {
        extra: users_with("http://ex/gb", false),
        ..Default::default()
    };
    std::fs::write(dir.path().join("auth.toml"), config_text(&f)).unwrap();
    st.auth.as_ref().unwrap().reload().unwrap();
    for user in ["aga", "aga2"] {
        let r = get_as(&app, &uri, Some(&b(user))).await;
        assert_ne!(
            r.status,
            StatusCode::OK,
            "{user} still follows: {}",
            r.text()
        );
    }
    let r = call(&app, "DELETE", &uri, &[("authorization", &b("aga"))], "").await;
    assert_ne!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    crate::http::algo::view::PAUSED.lock().retain(|d| d != "av");
    let done = crate::http::router_tests::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
}

#[tokio::test]
async fn a_branch_run_is_followed_on_its_branch() {
    let (_dir, st, app) = server();
    let r = call(
        &app,
        "POST",
        "/$/branches/av",
        &[
            ("authorization", &b("aadmin")),
            ("content-type", "application/json"),
        ],
        r#"{"name": "dev"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
        "async": true,
    });
    let r = call(
        &app,
        "POST",
        "/$/algo/av/run?branch=dev",
        &[
            ("authorization", &b("adev")),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(r.json()["dataset"], "av@dev", "{}", r.text());
    let uri = format!("/$/tasks/{id}");
    // the reader of the branch follows it, and a reader of main only does not see it
    let r = get_as(&app, &uri, Some(&b("adev"))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get_as(&app, &uri, Some(&b("amain"))).await;
    assert_ne!(r.status, StatusCode::OK, "{}", r.text());
    let done = crate::http::router_tests::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
}

#[tokio::test]
async fn memberships_take_a_grant_through_the_query_endpoint() {
    assert_eq!(
        crate::auth::routes::endpoint(
            "/$/graphrag/{ds}/memberships",
            &axum::http::Method::POST,
            &"/$/graphrag/av/memberships".parse().unwrap(),
            &axum::http::HeaderMap::new(),
        ),
        Some(crate::auth::Endpoint::Query)
    );
}
