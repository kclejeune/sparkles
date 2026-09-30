//! Authentication and dataset-level authorization through the router.

use super::*;
use crate::auth::{Auth, hash_password_with, token_hash};
use axum::http::HeaderMap;
use base64::Engine;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

fn tok(c: char) -> String {
    format!("spk_{}", c.to_string().repeat(43))
}

fn t_etl() -> String {
    tok('A')
}
fn t_prom() -> String {
    tok('B')
}
fn t_old() -> String {
    tok('C')
}

/// The fixture policy: anonymous reads `public`; alice is server-admin; bob writes
/// `wiki` and reads `team-*`; carol administers `wiki` and `wiki-*`; three tokens.
fn config_text(bob_wiki: bool) -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let bob_ds = if bob_wiki {
        r#"{ wiki = "write", "team-*" = "read" }"#
    } else {
        r#"{ "team-*" = "read" }"#
    };
    format!(
        r#"
version = 1
[anonymous]
datasets = {{ public = "read" }}

[[users]]
name = "alice"
password = "{alice}"
server = ["server-admin"]

[[users]]
name = "bob"
password = "{bob}"
datasets = {bob_ds}

[[users]]
name = "carol"
password = "{carol}"
datasets = {{ wiki = "admin", "wiki-*" = "admin" }}

[[tokens]]
name = "etl"
hash = "{etl}"
datasets = {{ wiki = "write" }}

[[tokens]]
name = "prometheus"
hash = "{prom}"
server = ["metrics"]

[[tokens]]
name = "old"
hash = "{old}"
datasets = {{ wiki = "read" }}
expires = "2020-01-01T00:00:00Z"

[cors]
origins = ["https://yasgui.example"]
"#,
        alice = h("alice-pw"),
        bob = h("bob-pw"),
        carol = h("carol-pw"),
        etl = token_hash(&t_etl()),
        prom = token_hash(&t_prom()),
        old = token_hash(&t_old()),
    )
}

struct AuthServer {
    _dir: tempfile::TempDir,
    config: PathBuf,
    state: Arc<AppState>,
    app: Router,
}

fn auth_server_with(enabled: bool, read_only: bool) -> AuthServer {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = read_only;
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(true)).unwrap();
    if enabled {
        st.auth = Some(Arc::new(Auth::load(&config).unwrap().0));
    }
    let st = Arc::new(st);
    for name in ["wiki", "team-a", "secret", "public"] {
        let ds = st.attach(name, DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                format!("<http://ex.org/{name}> <http://ex.org/p> \"1\" .").into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(crate::obs::Phase::Ready);
    let app = router(st.clone());
    AuthServer {
        _dir: dir,
        config,
        state: st,
        app,
    }
}

fn auth_server() -> AuthServer {
    auth_server_with(true, false)
}

fn basic(user: &str, pw: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}"))
    )
}

fn b(user: &str) -> String {
    basic(user, &format!("{user}-pw"))
}

fn bearer(t: &str) -> String {
    format!("Bearer {t}")
}

struct R {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl R {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn all(&self, name: &str) -> Vec<String> {
        self.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }
}

async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> R {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    R {
        status,
        headers,
        body,
    }
}

async fn get_as(app: &Router, uri: &str, auth: Option<&str>) -> R {
    match auth {
        Some(a) => call(app, "GET", uri, &[("authorization", a)], "").await,
        None => call(app, "GET", uri, &[], "").await,
    }
}

async fn update_as(app: &Router, ds: &str, auth: &str, update: &str) -> R {
    call(
        app,
        "POST",
        &format!("/{ds}/update"),
        &[
            ("authorization", auth),
            ("content-type", "application/sparql-update"),
        ],
        update,
    )
    .await
}

const ASK: &str = "/sparql?query=ASK%7B%7D";

fn head(st: &AppState, ds: &str) -> u64 {
    st.get(ds).unwrap().store.head_commit().seq
}

fn names(v: &J) -> Vec<String> {
    let mut n: Vec<String> = v["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

// ----------------------------------------------------------------------- A1 ------

#[tokio::test]
async fn a1_auth_disabled_is_unchanged() {
    let s = auth_server_with(false, false);
    let r = get_as(&s.app, &format!("/wiki{ASK}"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.all("www-authenticate").is_empty());
    let d = get_as(&s.app, "/$/datasets", None).await.json();
    assert_eq!(names(&d).len(), 4);
    assert!(d["datasets"][0].get("access").is_none());
    let w = get_as(&s.app, "/$/auth/whoami", None).await.json();
    assert_eq!(w["authEnabled"], false);
    assert_eq!(w["principal"]["kind"], "local");
    assert_eq!(w["datasets"]["wiki"], "admin");
    let pre = call(
        &s.app,
        "OPTIONS",
        "/wiki/sparql",
        &[
            ("origin", "https://x.example"),
            ("access-control-request-method", "POST"),
        ],
        "",
    )
    .await;
    assert_eq!(pre.all("access-control-allow-credentials"), vec!["true"]);
    let sv = get_as(&s.app, "/$/server", None).await.json();
    assert_eq!(sv["auth"]["enabled"], false);
}

// ----------------------------------------------------------------------- A2 ------

#[tokio::test]
async fn a2_anonymous() {
    let s = auth_server();
    let r = get_as(&s.app, &format!("/public{ASK}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let wiki = get_as(&s.app, &format!("/wiki{ASK}"), None).await;
    assert_eq!(wiki.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        wiki.all("www-authenticate"),
        vec![
            "Bearer realm=\"sparkles\"",
            "Basic realm=\"sparkles\", charset=\"UTF-8\""
        ]
    );
    assert_eq!(
        wiki.json(),
        serde_json::json!({"error": "authentication required"})
    );
    let nope = get_as(&s.app, &format!("/nope{ASK}"), None).await;
    assert_eq!(nope.status, wiki.status);
    assert_eq!(nope.all("www-authenticate"), wiki.all("www-authenticate"));
    assert_eq!(nope.body, wiki.body);
    // a script fetch gets no Basic challenge (no browser login dialog)
    let script = call(
        &s.app,
        "GET",
        &format!("/wiki{ASK}"),
        &[("sec-fetch-mode", "cors")],
        "",
    )
    .await;
    assert_eq!(
        script.all("www-authenticate"),
        vec!["Bearer realm=\"sparkles\""]
    );
}

// ----------------------------------------------------------------------- A3 ------

#[tokio::test]
async fn a3_invalid_credentials_are_not_anonymous() {
    let s = auth_server();
    let wrong = format!("spk_{}", "x".repeat(43));
    let r = get_as(&s.app, &format!("/public{ASK}"), Some(&bearer(&wrong))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.all("www-authenticate")[0].contains("error=\"invalid_token\""));
    let bad = get_as(
        &s.app,
        &format!("/public{ASK}"),
        Some(&basic("bob", "nope")),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert_eq!(bad.json()["error"], "invalid credentials");
    let unknown = get_as(
        &s.app,
        &format!("/public{ASK}"),
        Some(&basic("nobody", "x")),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.body, bad.body);
    let malformed = get_as(&s.app, &format!("/public{ASK}"), Some("Basic !!!")).await;
    assert_eq!(malformed.status, StatusCode::UNAUTHORIZED);
    let other = get_as(&s.app, &format!("/public{ASK}"), Some("Digest x")).await;
    assert_eq!(other.status, StatusCode::UNAUTHORIZED);
    // A14: failures are counted
    let m = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    assert!(
        m.contains("sparkles_auth_failures_total{scheme=\"bearer\",reason=\"invalid\"} 1"),
        "{m}"
    );
    assert!(
        m.contains("sparkles_auth_failures_total{scheme=\"basic\",reason=\"malformed\"} 1"),
        "{m}"
    );
    assert!(m.contains("outcome=\"denied\""), "{m}");
}

// ----------------------------------------------------------------------- A4 ------

#[tokio::test]
async fn a4_hidden_versus_forbidden() {
    let s = auth_server();
    let bob = b("bob");
    let ok = update_as(&s.app, "wiki", &bob, "INSERT DATA { <a:a> <a:b> <a:c> }").await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    let team = update_as(&s.app, "team-a", &bob, "INSERT DATA { <a:a> <a:b> <a:c> }").await;
    assert_eq!(team.status, StatusCode::FORBIDDEN);
    assert_eq!(team.json()["error"], "write access to /team-a required");
    let secret = get_as(&s.app, &format!("/secret{ASK}"), Some(&bob)).await;
    assert_eq!(secret.status, StatusCode::NOT_FOUND);
    assert_eq!(
        secret.json(),
        serde_json::json!({"error": "no such dataset: /secret"})
    );
    let nope = get_as(&s.app, &format!("/nope{ASK}"), Some(&bob)).await;
    assert_eq!(nope.status, StatusCode::NOT_FOUND);
    assert_eq!(
        nope.text().replace("nope", "X"),
        secret.text().replace("secret", "X")
    );
    let zzz = get_as(&s.app, &format!("/team-zzz{ASK}"), Some(&bob)).await;
    assert_eq!(zzz.status, StatusCode::NOT_FOUND);
    let del = call(
        &s.app,
        "DELETE",
        "/$/datasets/secret",
        &[("authorization", &bob)],
        "",
    )
    .await;
    assert_eq!(del.status, StatusCode::NOT_FOUND);
    assert_eq!(del.body, secret.body);
    // a missing dataset that the caller could administer: the handler's 404, same body
    let alice_del = call(
        &s.app,
        "DELETE",
        "/$/datasets/nope",
        &[("authorization", &b("alice"))],
        "",
    )
    .await;
    assert_eq!(alice_del.status, StatusCode::NOT_FOUND);
    assert_eq!(alice_del.body, nope.body);
    let m = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    assert!(
        m.contains("sparkles_auth_denied_total{kind=\"hidden\"} 3"),
        "{m}"
    );
    assert!(
        m.contains("sparkles_auth_denied_total{kind=\"forbidden\"} 1"),
        "{m}"
    );
}

// ----------------------------------------------------------------------- A5 ------

#[tokio::test]
async fn a5_filtered_listings() {
    let s = auth_server();
    let bob = get_as(&s.app, "/$/datasets", Some(&b("bob"))).await.json();
    assert_eq!(names(&bob), ["team-a", "wiki"]);
    let access: Vec<&str> = bob["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["access"].as_str().unwrap())
        .collect();
    assert_eq!(access, ["read", "write"]);
    let anon = get_as(&s.app, "/$/datasets", None).await.json();
    assert_eq!(names(&anon), ["public"]);
    let alice = get_as(&s.app, "/$/datasets", Some(&b("alice")))
        .await
        .json();
    assert_eq!(names(&alice).len(), 4);
    assert!(
        alice["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["access"] == "admin")
    );
    let server = get_as(&s.app, "/$/server", None).await;
    assert_eq!(server.status, StatusCode::OK);
    let sj = server.json();
    assert_eq!(names(&sj), ["public"]);
    assert_eq!(sj["auth"]["enabled"], true);
    assert_eq!(server.all("cache-control"), vec!["no-store"]);
    let one = get_as(&s.app, "/$/datasets/wiki", Some(&b("bob")))
        .await
        .json();
    assert_eq!(one["access"], "write");
}

// ----------------------------------------------------------------------- A6 ------

#[tokio::test]
async fn a6_token_via_bearer_and_basic() {
    let s = auth_server();
    for auth in [bearer(&t_etl()), basic("anything", &t_etl())] {
        let r = update_as(&s.app, "wiki", &auth, "INSERT DATA { <a:a> <a:b> <a:c> }").await;
        assert_eq!(r.status, StatusCode::OK, "{auth}: {}", r.text());
        let t = get_as(&s.app, &format!("/team-a{ASK}"), Some(&auth)).await;
        assert_eq!(t.status, StatusCode::NOT_FOUND);
    }
    // insufficient scope for a bearer caller
    let r = call(
        &s.app,
        "POST",
        "/$/compact/wiki",
        &[("authorization", &bearer(&t_etl()))],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.all("www-authenticate")[0].contains("insufficient_scope"));
}

// ----------------------------------------------------------------------- A7 ------

#[tokio::test]
async fn a7_form_post_is_rechecked() {
    let s = auth_server();
    let before = head(&s.state, "team-a");
    let form = [
        ("authorization", b("bob")),
        (
            "content-type",
            "application/x-www-form-urlencoded".to_string(),
        ),
    ];
    let h: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = call(
        &s.app,
        "POST",
        "/team-a",
        &h,
        "update=INSERT%20DATA%20%7B%3Ca%3Aa%3E%20%3Ca%3Ab%3E%20%3Ca%3Ac%3E%7D",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(head(&s.state, "team-a"), before);
    let q = call(&s.app, "POST", "/team-a", &h, "query=ASK%7B%7D").await;
    assert_eq!(q.status, StatusCode::OK, "{}", q.text());
    // bob may write wiki through the same form
    let w = call(
        &s.app,
        "POST",
        "/wiki",
        &h,
        "update=INSERT%20DATA%20%7B%3Ca%3Aa%3E%20%3Ca%3Ab%3E%20%3Ca%3Ac%3E%7D",
    )
    .await;
    assert_eq!(w.status, StatusCode::OK, "{}", w.text());
}

// ----------------------------------------------------------------------- A8 ------

#[tokio::test]
async fn a8_no_update_over_get() {
    for enabled in [true, false] {
        let s = auth_server_with(enabled, false);
        let before = head(&s.state, "wiki");
        let r = get_as(&s.app, "/wiki?update=CLEAR%20ALL", Some(&b("alice"))).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{}", r.text());
        assert_eq!(head(&s.state, "wiki"), before);
        assert_eq!(s.state.get("wiki").unwrap().store.snapshot().len(), 1);
    }
}

// ----------------------------------------------------------------------- A9 ------

async fn wait_task(st: &AppState, id: &str) -> crate::state::Task {
    let t0 = std::time::Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if t.state != "running" {
            return t;
        }
        assert!(t0.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a9_admin_operations_and_clone_target() {
    let s = auth_server();
    let post = |path: &'static str, who: &'static str| {
        let app = s.app.clone();
        async move { call(&app, "POST", path, &[("authorization", &b(who))], "").await }
    };
    assert_eq!(
        post("/$/compact/wiki", "bob").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post("/$/compact/wiki", "carol").await.status,
        StatusCode::ACCEPTED
    );
    let create = call(
        &s.app,
        "POST",
        "/$/datasets",
        &[
            ("authorization", &b("carol")),
            ("content-type", "application/json"),
        ],
        r#"{"dbName":"x"}"#,
    )
    .await;
    assert_eq!(create.status, StatusCode::FORBIDDEN);
    assert_eq!(create.json()["error"], "server-admin permission required");
    let clone = post("/$/datasets/wiki/clone?name=wiki-sandbox", "carol").await;
    assert_eq!(clone.status, StatusCode::ACCEPTED, "{}", clone.text());
    let t = wait_task(&s.state, clone.json()["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done", "{:?}", t.message);
    let list = get_as(&s.app, "/$/datasets", Some(&b("carol")))
        .await
        .json();
    let sandbox = list["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "wiki-sandbox")
        .cloned()
        .unwrap();
    assert_eq!(sandbox["access"], "admin");
    let prod = post("/$/datasets/wiki/clone?name=prod", "carol").await;
    assert_eq!(prod.status, StatusCode::FORBIDDEN);
    assert_eq!(
        prod.json()["error"],
        "no admin access to the target name /prod"
    );
    let bob = post("/$/datasets/wiki/clone?name=wiki-2", "bob").await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------- A10 ------

#[tokio::test]
async fn a10_tasks_are_filtered() {
    let s = auth_server();
    let r = call(
        &s.app,
        "POST",
        "/$/compact/secret",
        &[("authorization", &b("alice"))],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let id = r.json()["id"].as_str().unwrap().to_string();
    let bob = get_as(&s.app, "/$/tasks", Some(&b("bob"))).await.json();
    assert!(
        bob.as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != id.as_str())
    );
    let one = get_as(&s.app, &format!("/$/tasks/{id}"), Some(&b("bob"))).await;
    assert_eq!(one.status, StatusCode::NOT_FOUND);
    assert_eq!(one.json()["error"], "no such task");
    let alice = get_as(&s.app, &format!("/$/tasks/{id}"), Some(&b("alice"))).await;
    assert_eq!(alice.status, StatusCode::OK);
    let anon = get_as(&s.app, "/$/tasks", None).await;
    assert_eq!(anon.status, StatusCode::OK);
    assert!(anon.json().as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------- A11 ------

#[tokio::test]
async fn a11_server_routes() {
    let s = auth_server();
    assert_eq!(
        get_as(&s.app, "/$/metrics", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let bob = get_as(&s.app, "/$/metrics", Some(&b("bob"))).await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN);
    assert_eq!(bob.json()["error"], "metrics permission required");
    let prom = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom()))).await;
    assert_eq!(prom.status, StatusCode::OK);
    assert!(
        prom.text()
            .contains("# TYPE sparkles_requests_total counter")
    );
    assert_eq!(get_as(&s.app, "/$/ping", None).await.status, StatusCode::OK);
    let ready = get_as(&s.app, "/$/ready", None).await;
    assert_eq!(ready.status, StatusCode::OK);
    assert_eq!(names(&ready.json()), ["public"]);
    let all = get_as(&s.app, "/$/ready", Some(&bearer(&t_prom())))
        .await
        .json();
    assert_eq!(names(&all).len(), 4);
    let secret = get_as(&s.app, "/$/ready/secret", Some(&b("bob"))).await;
    assert_eq!(secret.status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------- A12 ------

/// A local HTTP listener that counts connections and answers 500.
fn counting_listener() -> (u16, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            n2.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = c.set_read_timeout(Some(Duration::from_millis(200)));
            let _ = c.read(&mut buf);
            let _ = c.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        }
    });
    (port, n)
}

#[tokio::test]
async fn a12_service_and_load_need_permissions() {
    let s = auth_server();
    let (port, conns) = counting_listener();
    let q = format!("SELECT * {{ SERVICE <http://127.0.0.1:{port}/x> {{ ?s ?p ?o }} }}");
    let uri = format!(
        "/wiki/sparql?query={}",
        percent_encoding::utf8_percent_encode(&q, percent_encoding::NON_ALPHANUMERIC)
    );
    let bob = get_as(&s.app, &uri, Some(&b("bob"))).await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN, "{}", bob.text());
    assert_eq!(
        bob.json()["error"],
        "SERVICE requires the federate permission"
    );
    assert_eq!(conns.load(Ordering::SeqCst), 0);
    // SILENT does not hide a refusal
    let silent = q.replace("SERVICE", "SERVICE SILENT");
    let uri2 = format!(
        "/wiki/sparql?query={}",
        percent_encoding::utf8_percent_encode(&silent, percent_encoding::NON_ALPHANUMERIC)
    );
    assert_eq!(
        get_as(&s.app, &uri2, Some(&b("bob"))).await.status,
        StatusCode::FORBIDDEN
    );
    let alice = get_as(&s.app, &uri, Some(&b("alice"))).await;
    assert_ne!(alice.status, StatusCode::FORBIDDEN, "{}", alice.text());
    assert_eq!(conns.load(Ordering::SeqCst), 1);

    let before = head(&s.state, "wiki");
    let file = update_as(&s.app, "wiki", &b("bob"), "LOAD <file:///etc/hostname>").await;
    assert_eq!(file.status, StatusCode::FORBIDDEN, "{}", file.text());
    assert_eq!(file.json()["error"], "LOAD <file:…> requires server-admin");
    let silent = update_as(
        &s.app,
        "wiki",
        &b("bob"),
        "LOAD SILENT <file:///etc/hostname>",
    )
    .await;
    assert_eq!(silent.status, StatusCode::FORBIDDEN);
    let http = update_as(
        &s.app,
        "wiki",
        &b("bob"),
        &format!("LOAD <http://127.0.0.1:{port}/d.ttl>"),
    )
    .await;
    assert_eq!(http.status, StatusCode::FORBIDDEN);
    assert_eq!(head(&s.state, "wiki"), before);
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_sparql_constraints_cannot_reach_out() {
    let s = auth_server();
    let (port, conns) = counting_listener();
    let shapes = format!(
        r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
<urn:s> a sh:NodeShape ; sh:targetNode <http://ex.org/wiki> ;
  sh:sparql [ sh:select "SELECT $this WHERE {{ SERVICE <http://127.0.0.1:{port}/x> {{ ?a ?b ?c }} }}" ] ."#
    );
    let r = call(
        &s.app,
        "POST",
        "/wiki/shacl",
        &[
            ("authorization", &b("bob")),
            ("content-type", "text/turtle"),
        ],
        &shapes,
    )
    .await;
    // SHACL-SPARQL refuses SERVICE for everyone
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(conns.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------- A13 ------

#[tokio::test]
async fn a13_cors_and_csrf() {
    let s = auth_server();
    let pre = |origin: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "OPTIONS",
                "/wiki/sparql",
                &[
                    ("origin", origin),
                    ("access-control-request-method", "POST"),
                    ("access-control-request-headers", "authorization"),
                ],
                "",
            )
            .await
        }
    };
    let evil = pre("https://evil.example").await;
    assert!(evil.all("access-control-allow-origin").is_empty());
    let ok = pre("https://yasgui.example").await;
    assert_eq!(
        ok.all("access-control-allow-origin"),
        vec!["https://yasgui.example"]
    );
    assert!(ok.all("access-control-allow-headers")[0].contains("authorization"));
    assert!(ok.all("access-control-allow-credentials").is_empty());

    let before = head(&s.state, "wiki");
    let bob = b("bob");
    let cross = call(
        &s.app,
        "POST",
        "/wiki/update",
        &[
            ("authorization", &bob),
            ("content-type", "application/sparql-update"),
            ("origin", "https://evil.example"),
            ("host", "localhost:3030"),
        ],
        "INSERT DATA { <a:a> <a:b> <a:c> }",
    )
    .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    assert_eq!(cross.json()["error"], "cross-origin request refused");
    assert_eq!(head(&s.state, "wiki"), before);
    let same = call(
        &s.app,
        "POST",
        "/wiki/update",
        &[
            ("authorization", &bob),
            ("content-type", "application/sparql-update"),
            ("origin", "http://localhost:3030"),
            ("host", "localhost:3030"),
        ],
        "INSERT DATA { <a:a> <a:b> <a:c> }",
    )
    .await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.text());
    let read = call(
        &s.app,
        "GET",
        &format!("/wiki{ASK}"),
        &[("authorization", &bob), ("sec-fetch-site", "cross-site")],
        "",
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);
}

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Buf(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[test]
fn a13_logs_carry_principals_never_secrets() {
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(buf.clone())
        .finish();
    let s = auth_server();
    let hash = token_hash(&t_etl());
    tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob"))).await;
                get_as(&s.app, &format!("/wiki{ASK}"), Some(&basic("bob", "wrong"))).await;
                get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&t_etl()))).await;
                get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&tok('Z')))).await;
                get_as(&s.app, &format!("/secret{ASK}"), Some(&b("bob"))).await;
                get_as(&s.app, &format!("/wiki{ASK}"), None).await;
            });
    });
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    for secret in ["bob-pw", &t_etl(), &tok('Z'), &hash, "$argon2id$", "wrong"] {
        assert!(!text.contains(secret), "{secret} in the log:\n{text}");
    }
    assert!(
        !text.to_ascii_lowercase().contains("authorization"),
        "{text}"
    );
    let access: Vec<J> = text
        .lines()
        .filter(|l| l.contains(r#""target":"sparkles::access""#))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(access.len(), 6, "{text}");
    let principals: Vec<&str> = access
        .iter()
        .map(|j| j["fields"]["principal"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        principals,
        ["user:bob", "-", "token:etl", "-", "user:bob", "anonymous"]
    );
    assert_eq!(access[0]["span"]["principal"], "user:bob");
    assert_eq!(access[1]["fields"]["auth_error"], "invalid");
    assert_eq!(access[4]["fields"]["outcome"], "denied");
    assert_eq!(access[4]["fields"]["status"], 404);
}

// ---------------------------------------------------------------------- A15 ------

#[tokio::test]
async fn a15_expired_token() {
    let s = auth_server();
    let r = get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&t_old()))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.all("www-authenticate")[0].contains("error_description=\"token expired\""));
    let m = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    assert!(m.contains("sparkles_auth_failures_total{scheme=\"bearer\",reason=\"expired\"} 1"));
}

// ---------------------------------------------------------------------- A16 ------

#[tokio::test]
async fn a16_reload() {
    let s = auth_server();
    let auth = s.state.auth.clone().unwrap();
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob")))
            .await
            .status,
        StatusCode::OK
    );
    std::fs::write(&s.config, config_text(false)).unwrap();
    auth.reload().unwrap();
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob")))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    std::fs::write(
        &s.config,
        format!("{}\ndataset = {{}}\n", config_text(true)),
    )
    .unwrap();
    assert!(auth.reload().is_err());
    // the previous policy stays
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob")))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let m = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    assert!(
        m.contains("sparkles_auth_reloads_total{result=\"ok\"} 1"),
        "{m}"
    );
    assert!(
        m.contains("sparkles_auth_reloads_total{result=\"error\"} 1"),
        "{m}"
    );
}

// ---------------------------------------------------------------------- A17 ------

#[tokio::test]
async fn a17_read_only_applies_after_auth() {
    let s = auth_server_with(true, true);
    let alice = call(
        &s.app,
        "POST",
        "/$/compact/wiki",
        &[("authorization", &b("alice"))],
        "",
    )
    .await;
    assert_eq!(alice.status, StatusCode::FORBIDDEN);
    assert_eq!(alice.json()["error"], "server is read-only");
    let anon = call(&s.app, "POST", "/$/compact/wiki", &[], "").await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------- A18 ------

#[tokio::test]
async fn a18_whoami() {
    let s = auth_server();
    let bob = get_as(&s.app, "/$/auth/whoami", Some(&b("bob")))
        .await
        .json();
    assert_eq!(
        bob,
        serde_json::json!({
            "authEnabled": true,
            "principal": { "kind": "user", "name": "bob" },
            "server": [],
            "datasets": { "team-a": "read", "wiki": "write" }
        })
    );
    let anon = get_as(&s.app, "/$/auth/whoami", None).await.json();
    assert_eq!(
        anon["principal"],
        serde_json::json!({ "kind": "anonymous" })
    );
    assert_eq!(anon["datasets"], serde_json::json!({ "public": "read" }));
    let bad = get_as(&s.app, "/$/auth/whoami", Some(&basic("bob", "x"))).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    let alice = get_as(&s.app, "/$/auth/whoami", Some(&b("alice")))
        .await
        .json();
    assert_eq!(alice["server"], serde_json::json!(["server-admin"]));
}

// ---------------------------------------------------------------------- A19 ------

/// Every `.route(…)` of `router()` (and the auth routes) is in the route table, with a
/// need for each method it serves.
#[test]
fn a19_route_coverage() {
    let src = include_str!("../../http.rs");
    let start = src.find("Router::new()").unwrap();
    let end = start + src[start..].find(".layer(").unwrap();
    let body = &src[start..end];
    let method_re = regex::Regex::new(r"\b(get|post|put|delete|head|any)\(").unwrap();
    let mut found = 0;
    for chunk in body.split(".route(").skip(1) {
        let template = chunk.split('"').nth(1).unwrap();
        let methods: Vec<String> = method_re
            .captures_iter(chunk)
            .map(|c| c[1].to_ascii_uppercase())
            .collect();
        let listed = crate::auth::ROUTES
            .iter()
            .find(|(t, _)| *t == template)
            .unwrap_or_else(|| panic!("route {template} is not in auth::ROUTES"));
        for m in &methods {
            if m == "ANY" {
                assert_eq!(listed.1, &["*"], "{template}");
                continue;
            }
            assert!(
                listed.1.contains(&m.as_str()) || listed.1 == ["*"],
                "{template} {m} is not in auth::ROUTES"
            );
            let method: axum::http::Method = m.parse().unwrap();
            assert!(
                crate::auth::need(template, &method, &"/x".parse().unwrap(), &HeaderMap::new())
                    .is_some(),
                "{template} {m} has no need"
            );
        }
        found += 1;
    }
    assert!(found > 30, "{found}");
    // and nothing unknown slips through
    assert_eq!(
        crate::auth::need(
            "/$/new-thing",
            &axum::http::Method::GET,
            &"/".parse().unwrap(),
            &HeaderMap::new()
        ),
        None
    );
}

// ---------------------------------------------------------------------- A20 ------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a20_login_cost_is_bounded() {
    let s = auth_server();
    let auth = s.state.auth.clone().unwrap();
    let verifications = || auth.metrics.password_verifications.load(Ordering::Relaxed);
    let before = verifications();
    let mut handles = Vec::new();
    for _ in 0..50 {
        let app = s.app.clone();
        handles.push(tokio::spawn(async move {
            get_as(&app, &format!("/wiki{ASK}"), Some(&b("bob")))
                .await
                .status
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(verifications() - before, 1);

    let before = verifications();
    let mut handles = Vec::new();
    for _ in 0..50 {
        let app = s.app.clone();
        handles.push(tokio::spawn(async move {
            get_as(&app, &format!("/wiki{ASK}"), Some(&basic("nobody", "x")))
                .await
                .status
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(verifications() - before, 50);
}

// ---------------------------------------------------------------------- A21 ------

#[test]
fn a21_hashes_and_tokens() {
    let h = crate::auth::hash_password("pw").unwrap();
    let re = regex::Regex::new(
        r"^\$argon2id\$v=19\$m=19456,t=2,p=1\$[A-Za-z0-9+/]{22}\$[A-Za-z0-9+/]{43}$",
    )
    .unwrap();
    assert!(re.is_match(&h), "{h}");
    let t = crate::auth::new_token();
    assert!(
        regex::Regex::new(r"^spk_[A-Za-z0-9_-]{43}$")
            .unwrap()
            .is_match(&t)
    );
    assert_eq!(token_hash(&t), token_hash(&t));
    assert!(
        crate::auth::load(
            Some(std::path::Path::new("/nonexistent/auth.toml")),
            "127.0.0.1"
        )
        .is_err()
    );
}
