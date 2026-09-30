//! Authentication and dataset-level authorization through the router.

use super::*;
use crate::auth::{Auth, Peer, hash_password_with, token_hash};
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use base64::Engine;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

mod cli_grants;
mod oidc;
mod proxy;
mod sessions;
mod tokens;

pub(super) fn tok(c: char) -> String {
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

/// Fixture options.
#[derive(Clone)]
pub(super) struct Fixture {
    pub enabled: bool,
    pub read_only: bool,
    /// bob's `wiki = "write"` grant
    pub bob_wiki: bool,
    /// bob's `"team-*" = "read"` grant
    pub bob_team: bool,
    pub public_url: &'static str,
    /// more TOML (`[oidc]`, `[proxy]`, …)
    pub extra: String,
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture {
            enabled: true,
            read_only: false,
            bob_wiki: true,
            bob_team: true,
            public_url: "http://localhost:3030",
            extra: String::new(),
        }
    }
}

/// The fixture policy: anonymous reads `public`; alice is server-admin; bob writes
/// `wiki` and reads `team-*`; carol administers `wiki` and `wiki-*`; static tokens
/// `etl` (writes wiki), `prometheus` (metrics) and `old` (expired); OIDC and proxy
/// identities in group `sparkles` are admitted, and `kg-editors` write `wiki`.
pub(super) fn config_text(f: &Fixture) -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let mut bob = Vec::new();
    if f.bob_wiki {
        bob.push(r#"wiki = "write""#);
    }
    if f.bob_team {
        bob.push(r#""team-*" = "read""#);
    }
    format!(
        r#"
version = 1

[server]
public_url = "{public_url}"

[anonymous]
datasets = {{ public = "read" }}

[roles.wiki-editors]
datasets = {{ wiki = "write" }}

[[users]]
name = "alice"
password = "{alice}"
server = ["server-admin"]

[[users]]
name = "bob"
password = "{bob_pw}"
datasets = {{ {bob} }}

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

[external]
allowed_groups = ["sparkles"]
[external.group_roles]
"kg-editors" = ["wiki-editors"]

[cors]
origins = ["https://yasgui.example"]
{extra}
"#,
        public_url = f.public_url,
        alice = h("alice-pw"),
        bob_pw = h("bob-pw"),
        bob = bob.join(", "),
        carol = h("carol-pw"),
        etl = token_hash(&t_etl()),
        prom = token_hash(&t_prom()),
        old = token_hash(&t_old()),
        extra = f.extra,
    )
}

pub(super) struct AuthServer {
    pub dir: tempfile::TempDir,
    pub config: PathBuf,
    pub state: Arc<AppState>,
    pub app: Router,
    pub fixture: Fixture,
}

impl AuthServer {
    pub fn auth(&self) -> Arc<Auth> {
        self.state.auth.clone().unwrap()
    }

    /// Rewrite the configuration file.
    pub fn write_config(&self, f: &Fixture) {
        std::fs::write(&self.config, config_text(f)).unwrap();
    }

    /// A new server state from the same data directory and configuration (a restart).
    pub fn restart(self) -> AuthServer {
        let AuthServer {
            dir,
            config,
            state,
            fixture,
            ..
        } = self;
        crate::auth::flush(&state);
        drop(state);
        let (state, app) = open_state(dir.path(), &config, &fixture);
        AuthServer {
            dir,
            config,
            state,
            app,
            fixture,
        }
    }
}

fn open_state(
    dir: &std::path::Path,
    config: &std::path::Path,
    f: &Fixture,
) -> (Arc<AppState>, Router) {
    let mut st = AppState::new(dir, StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = f.read_only;
    if f.enabled {
        st.auth = Some(Arc::new(Auth::open(config, dir).unwrap().0));
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
    (st, app)
}

pub(super) fn build(f: Fixture) -> AuthServer {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let (state, app) = open_state(dir.path(), &config, &f);
    AuthServer {
        dir,
        config,
        state,
        app,
        fixture: f,
    }
}

fn auth_server_with(enabled: bool, read_only: bool) -> AuthServer {
    build(Fixture {
        enabled,
        read_only,
        ..Default::default()
    })
}

pub(super) fn auth_server() -> AuthServer {
    auth_server_with(true, false)
}

pub(super) fn basic(user: &str, pw: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}"))
    )
}

pub(super) fn b(user: &str) -> String {
    basic(user, &format!("{user}-pw"))
}

pub(super) fn bearer(t: &str) -> String {
    format!("Bearer {t}")
}

pub(super) struct R {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl R {
    pub fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    /// The JSON error body without its `requestId` (which differs per request).
    pub fn err(&self) -> J {
        let mut j = self.json();
        assert!(j["requestId"].is_string(), "no requestId in {j}");
        j.as_object_mut().unwrap().remove("requestId");
        j
    }
    pub fn all(&self, name: &str) -> Vec<String> {
        self.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }
    pub fn header(&self, name: &str) -> String {
        self.all(name).into_iter().next().unwrap_or_default()
    }
    /// The `name=value` of the `Set-Cookie` for cookie `name` or `__Host-name`
    /// (without attributes).
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.set_cookie(name)
            .map(|c| c.split(';').next().unwrap().to_string())
    }
    /// The whole `Set-Cookie` value for cookie `name` or `__Host-name`.
    pub fn set_cookie(&self, name: &str) -> Option<String> {
        self.all("set-cookie").into_iter().find(|c| {
            c.starts_with(&format!("{name}=")) || c.starts_with(&format!("__Host-{name}="))
        })
    }
}

/// The peer of test requests unless stated: not a trusted proxy.
pub(super) fn default_peer() -> Peer {
    Peer::Tcp("127.0.0.2:1".parse().unwrap())
}

pub(super) async fn call_from(
    app: &Router,
    peer: Peer,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> R {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .extension(ConnectInfo(peer));
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

pub(super) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> R {
    call_from(app, default_peer(), method, uri, headers, body).await
}

pub(super) async fn get_as(app: &Router, uri: &str, auth: Option<&str>) -> R {
    match auth {
        Some(a) => call(app, "GET", uri, &[("authorization", a)], "").await,
        None => call(app, "GET", uri, &[], "").await,
    }
}

pub(super) async fn update_as(app: &Router, ds: &str, auth: &str, update: &str) -> R {
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

pub(super) const ASK: &str = "/sparql?query=ASK%7B%7D";
pub(super) const INSERT: &str = "INSERT DATA { <a:a> <a:b> <a:c> }";

pub(super) fn head(st: &AppState, ds: &str) -> u64 {
    st.get(ds).unwrap().store.head_commit().seq
}

pub(super) fn names(v: &J) -> Vec<String> {
    let mut n: Vec<String> = v["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

/// A UI session: `Cookie` value and CSRF token, from a password login as `user`.
pub(super) async fn password_session(s: &AuthServer, user: &str) -> (String, String) {
    let r = call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[
            ("content-type", "application/json"),
            ("origin", s.fixture.public_url),
        ],
        &format!(r#"{{"user":"{user}","password":"{user}-pw"}}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let cookie = r.cookie("sparkles_session").unwrap();
    let csrf = csrf_of(&s.app, &cookie).await;
    (cookie, csrf)
}

/// The whoami `csrfToken` of a session cookie.
pub(super) async fn csrf_of(app: &Router, cookie: &str) -> String {
    let who = call(app, "GET", "/$/whoami", &[("cookie", cookie)], "")
        .await
        .json();
    who["csrfToken"].as_str().unwrap().to_string()
}

/// The value of an `sparkles_auth_*` or other metric line.
pub(super) async fn metric(app: &Router, line_prefix: &str) -> u64 {
    let m = get_as(app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    m.lines()
        .find(|l| l.starts_with(line_prefix))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no metric {line_prefix} in\n{m}"))
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_disabled_is_unchanged() {
    let s = auth_server_with(false, false);
    let r = get_as(&s.app, &format!("/wiki{ASK}"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.all("www-authenticate").is_empty());
    let d = get_as(&s.app, "/$/datasets", None).await.json();
    assert_eq!(names(&d).len(), 4);
    assert!(d["datasets"][0].get("access").is_none());
    let w = get_as(&s.app, "/$/whoami", None).await.json();
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous() {
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
        wiki.err(),
        serde_json::json!({"error": "authentication required"})
    );
    let nope = get_as(&s.app, &format!("/nope{ASK}"), None).await;
    assert_eq!(nope.status, wiki.status);
    assert_eq!(nope.all("www-authenticate"), wiki.all("www-authenticate"));
    assert_eq!(nope.err(), wiki.err());
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_credentials_are_not_anonymous() {
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
    assert_eq!(unknown.err(), bad.err());
    let malformed = get_as(&s.app, &format!("/public{ASK}"), Some("Basic !!!")).await;
    assert_eq!(malformed.status, StatusCode::UNAUTHORIZED);
    let other = get_as(&s.app, &format!("/public{ASK}"), Some("Digest x")).await;
    assert_eq!(other.status, StatusCode::UNAUTHORIZED);
    // failures are counted
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn hidden_versus_forbidden() {
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
        secret.err(),
        serde_json::json!({"error": "no such dataset: /secret"})
    );
    let nope = get_as(&s.app, &format!("/nope{ASK}"), Some(&bob)).await;
    assert_eq!(nope.status, StatusCode::NOT_FOUND);
    assert_eq!(
        nope.err().to_string().replace("nope", "X"),
        secret.err().to_string().replace("secret", "X")
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
    assert_eq!(del.err(), secret.err());
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
    assert_eq!(alice_del.err(), nope.err());
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn filtered_listings() {
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn token_via_bearer_and_basic() {
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn form_post_is_rechecked() {
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

/// A static token that reads `team-a` and `public` and nothing else.
fn reader_fixture() -> Fixture {
    Fixture {
        extra: format!(
            "[[tokens]]\nname = \"reader\"\nhash = \"{}\"\ndatasets = {{ \"team-a\" = \"read\", public = \"read\" }}\n",
            token_hash(&tok('R'))
        ),
        ..Default::default()
    }
}

const FORM: &str = "application/x-www-form-urlencoded";
const TRIPLE: &str = "<urn:s> <urn:p> <urn:o> .";

#[tokio::test]
async fn form_body_without_an_operation_is_not_an_upload() {
    let s = build(reader_fixture());
    let reader = bearer(&tok('R'));
    let post = |uri: &'static str, auth: Option<String>, ct: &'static str| {
        let app = s.app.clone();
        async move {
            let mut h = vec![("content-type", ct.to_string())];
            h.extend(auth.map(|a| ("authorization", a)));
            let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            call(&app, "POST", uri, &h, TRIPLE).await
        }
    };
    let before = head(&s.state, "team-a");
    // a read-only caller gets the answer of any other write: 403, same body and headers
    let form = post("/team-a?format=turtle", Some(reader.clone()), FORM).await;
    let turtle = post("/team-a?format=turtle", Some(reader.clone()), "text/turtle").await;
    assert_eq!(form.status, StatusCode::FORBIDDEN, "{}", form.text());
    assert_eq!(form.err(), turtle.err());
    assert_eq!(form.err()["error"], "write access to /team-a required");
    assert_eq!(form.all("www-authenticate"), turtle.all("www-authenticate"));
    for g in [
        "/team-a?format=turtle&default",
        "/team-a?format=nt&graph=urn:g",
    ] {
        let r = post(g, Some(reader.clone()), FORM).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{g}: {}", r.text());
    }
    assert_eq!(head(&s.state, "team-a"), before);
    assert_eq!(s.state.get("team-a").unwrap().store.snapshot().len(), 1);
    // without any access the dataset stays hidden; anonymous callers are asked to sign in
    let hidden = post("/secret?format=turtle", Some(reader.clone()), FORM).await;
    let hidden_turtle = post("/secret?format=turtle", Some(reader.clone()), "text/turtle").await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(hidden.err(), hidden_turtle.err());
    let anon = post("/public?format=turtle", None, FORM).await;
    let anon_turtle = post("/public?format=turtle", None, "text/turtle").await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anon.err(), anon_turtle.err());
    assert_eq!(
        anon.all("www-authenticate"),
        anon_turtle.all("www-authenticate")
    );
    // a caller who may write gets a plain 400: a form body is never RDF
    let etl = post("/wiki?format=turtle", Some(bearer(&t_etl())), FORM).await;
    assert_eq!(etl.status, StatusCode::BAD_REQUEST, "{}", etl.text());
    assert_eq!(etl.err()["error"], "missing 'query' or 'update' parameter");
    assert_eq!(s.state.get("wiki").unwrap().store.snapshot().len(), 1);
    // a form query still needs only read, a form update write
    let h = [("authorization", reader.as_str()), ("content-type", FORM)];
    let q = call(&s.app, "POST", "/team-a", &h, "query=ASK%7B%7D").await;
    assert_eq!(q.status, StatusCode::OK, "{}", q.text());
    let u = call(&s.app, "POST", "/team-a", &h, "update=CLEAR%20ALL").await;
    assert_eq!(u.status, StatusCode::FORBIDDEN, "{}", u.text());
    assert_eq!(u.err(), turtle.err());
    assert_eq!(head(&s.state, "team-a"), before);
    let m = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    assert!(
        m.contains("sparkles_auth_denied_total{kind=\"forbidden\"} 5"),
        "{m}"
    );
}

/// A read-only caller cannot change a dataset through any route, method, content type,
/// graph selector or body of the SPARQL and Graph Store endpoints.
#[tokio::test]
async fn read_access_never_writes() {
    let s = build(reader_fixture());
    let reader = bearer(&tok('R'));
    let ds = s.state.get("team-a").unwrap();
    let state = || {
        (
            ds.store.head_commit().seq,
            ds.store.snapshot().len(),
            ds.store.prefixes(),
        )
    };
    let before = state();
    let paths = [
        "",
        "/data",
        "/sparql",
        "/query",
        "/update",
        "/upload",
        "/get",
        "/prefixes",
        "/explain",
        "/shacl",
    ];
    let methods = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"];
    let queries = [
        "",
        "?format=turtle",
        "?default",
        "?graph=urn:g",
        "?graph=default&format=nt",
        "?query=ASK%7B%7D&format=turtle",
        "?update=INSERT%20DATA%20%7B%3Ca%3Aa%3E%20%3Ca%3Ab%3E%20%3Ca%3Ac%3E%7D",
        "?prefix=x&uri=urn:x",
    ];
    let form_update = "update=INSERT%20DATA%20%7B%3Ca%3Aa%3E%20%3Ca%3Ab%3E%20%3Ca%3Ac%3E%7D";
    let bodies: &[(&str, &[&str])] = &[
        ("", &[TRIPLE]),
        (
            FORM,
            &[TRIPLE, form_update, "prefix=x&uri=urn:x", "default="],
        ),
        ("text/turtle", &[TRIPLE]),
        (
            "application/n-quads",
            &["<urn:s> <urn:p> <urn:o> <urn:g> ."],
        ),
        ("application/sparql-update", &[INSERT]),
        ("application/sparql-query", &[TRIPLE]),
        ("application/json", &[r#"{"prefix":"x","uri":"urn:x"}"#]),
        ("multipart/form-data; boundary=X", &[TRIPLE]),
    ];
    let mut n = 0;
    for path in paths {
        for method in methods {
            for q in queries {
                for (ct, bs) in bodies {
                    for body in *bs {
                        let uri = format!("/team-a{path}{q}");
                        let mut h = vec![("authorization", reader.as_str())];
                        if !ct.is_empty() {
                            h.push(("content-type", ct));
                        }
                        let r = call(&s.app, method, &uri, &h, body).await;
                        let what = format!("{method} {uri} ({ct}): {}", r.status);
                        assert!(r.status != StatusCode::INTERNAL_SERVER_ERROR, "{what}");
                        assert!(
                            !r.status.is_success() || matches!(method, "GET" | "HEAD" | "POST"),
                            "{what}"
                        );
                        assert_eq!(state(), before, "{what}");
                        n += 1;
                    }
                }
            }
        }
    }
    assert!(n > 3000);
    // the same writes by a writer do go through (the table is not vacuous)
    let etl = bearer(&t_etl());
    let w = |m: &'static str, uri: &'static str, ct: &'static str| {
        let (app, etl) = (s.app.clone(), etl.clone());
        async move {
            let h = [("authorization", etl.as_str()), ("content-type", ct)];
            call(&app, m, uri, &h, TRIPLE).await.status
        }
    };
    assert_eq!(
        w("PUT", "/wiki/data?default", "text/turtle").await,
        StatusCode::OK
    );
    assert_eq!(
        w("POST", "/wiki?graph=urn:g", "text/turtle").await,
        StatusCode::CREATED
    );
    assert_eq!(
        w("POST", "/wiki", "application/n-triples").await,
        StatusCode::CREATED
    );
    assert_eq!(
        w("DELETE", "/wiki?graph=urn:g", "").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        w("POST", "/wiki/upload", "text/turtle").await,
        StatusCode::OK
    );
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_update_over_get() {
    for enabled in [true, false] {
        let s = auth_server_with(enabled, false);
        let before = head(&s.state, "wiki");
        let r = get_as(&s.app, "/wiki?update=CLEAR%20ALL", Some(&b("alice"))).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{}", r.text());
        assert_eq!(head(&s.state, "wiki"), before);
        assert_eq!(s.state.get("wiki").unwrap().store.snapshot().len(), 1);
    }
}

// ---------------------------------------------------------------------------

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
async fn admin_operations_and_clone_target() {
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn tasks_are_filtered() {
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_routes() {
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

// ---------------------------------------------------------------------------

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
async fn service_and_load_need_permissions() {
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn cors_and_csrf() {
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
fn logs_carry_principals_never_secrets() {
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
    let mut secrets: Vec<String> = Vec::new();
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
                // a session, a minted token, a device grant and a loopback exchange
                let (cookie, csrf) = password_session(&s, "alice").await;
                let v = cookie.split_once('=').unwrap().1;
                secrets.push(v.rsplit_once('.').unwrap().0.to_string());
                secrets.push(csrf.clone());
                let m = tokens::mint_as(&s.app, &[("cookie", &cookie), ("x-sparkles-csrf", &csrf)], r#"{"name":"x"}"#).await;
                secrets.push(m.json()["token"].as_str().unwrap().to_string());
                let d = cli_grants::form(&s.app, "/$/auth/device", "label=l").await.json();
                let dc = d["device_code"].as_str().unwrap().to_string();
                cli_grants::form(&s.app, "/$/auth/token", &format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={dc}")).await;
                secrets.push(dc);
                let verifier = crate::auth::crypto::random_token(32);
                let challenge = crate::auth::crypto::pkce_challenge(&verifier);
                let a = call(&s.app, "POST", "/$/auth/cli/authorize", &[("cookie", &cookie), ("x-sparkles-csrf", &csrf), ("content-type", "application/json")], &format!(r#"{{"port":50000,"state":"st","codeChallenge":"{challenge}"}}"#)).await.json();
                let redirect = a["redirect"].as_str().unwrap().to_string();
                let code = redirect.split("code=").nth(1).unwrap().split('&').next().unwrap().to_string();
                let t = cli_grants::form(&s.app, "/$/auth/token", &format!("grant_type=authorization_code&code={code}&code_verifier={verifier}")).await.json();
                secrets.push(t["access_token"].as_str().unwrap().to_string());
                secrets.push(code);
                secrets.push(verifier);
            });
    });
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    for secret in [
        "bob-pw",
        "alice-pw",
        &t_etl(),
        &tok('Z'),
        &hash,
        "$argon2id$",
        "wrong",
        "spk_",
        "sha256:",
    ]
    .into_iter()
    .chain(secrets.iter().map(String::as_str))
    {
        assert!(!text.contains(secret), "{secret} in the log:\n{text}");
    }
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains("authorization"), "{text}");
    assert!(!lower.contains("cookie"), "{text}");
    let access: Vec<J> = text
        .lines()
        .filter(|l| l.contains(r#""target":"sparkles::access""#))
        .map(|l| serde_json::from_str(l).unwrap())
        .take(6)
        .collect();
    assert_eq!(access.len(), 6, "{text}");
    assert!(text.contains(r#""event":"token_minted""#), "{text}");
    let principals: Vec<&str> = access
        .iter()
        .map(|j| j["fields"]["principal"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        principals,
        [
            "user:bob",
            "-",
            "token:cfg-etl",
            "-",
            "user:bob",
            "anonymous"
        ]
    );
    assert_eq!(access[0]["span"]["principal"], "user:bob");
    assert_eq!(access[1]["fields"]["auth_error"], "invalid");
    assert_eq!(access[4]["fields"]["outcome"], "denied");
    assert_eq!(access[4]["fields"]["status"], 404);
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_token() {
    let s = auth_server();
    let r = get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&t_old()))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.all("www-authenticate")[0].contains("error_description=\"token expired\""));
    let m = get_as(&s.app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text();
    assert!(m.contains("sparkles_auth_failures_total{scheme=\"bearer\",reason=\"expired\"} 1"));
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn reload() {
    let s = auth_server();
    let auth = s.state.auth.clone().unwrap();
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob")))
            .await
            .status,
        StatusCode::OK
    );
    s.write_config(&Fixture {
        bob_wiki: false,
        ..Default::default()
    });
    auth.reload().unwrap();
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob")))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    std::fs::write(
        &s.config,
        format!("dataset = {{}}\n{}", config_text(&Fixture::default())),
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_only_applies_after_auth() {
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn whoami_reports_permissions() {
    let s = auth_server();
    let r = get_as(&s.app, "/$/whoami", Some(&b("bob"))).await;
    assert_eq!(r.header("cache-control"), "no-store");
    let bob = r.json();
    assert_eq!(bob["authEnabled"], true);
    assert_eq!(
        bob["principal"],
        serde_json::json!({ "kind": "user", "name": "bob" })
    );
    assert_eq!(bob["method"], "basic");
    assert_eq!(bob["server"], serde_json::json!([]));
    assert_eq!(
        bob["datasets"],
        serde_json::json!({ "team-a": "read", "wiki": "write" })
    );
    assert_eq!(bob["canMintTokens"], true);
    assert_eq!(bob["logout"], false);
    // Basic is not ambient in the synchronizer sense: no CSRF token
    assert!(bob.get("csrfToken").is_none());
    let anon = get_as(&s.app, "/$/whoami", None).await.json();
    assert_eq!(
        anon["principal"],
        serde_json::json!({ "kind": "anonymous" })
    );
    assert_eq!(anon["datasets"], serde_json::json!({ "public": "read" }));
    let bad = get_as(&s.app, "/$/whoami", Some(&basic("bob", "x"))).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    let alice = get_as(&s.app, "/$/whoami", Some(&b("alice"))).await.json();
    assert_eq!(alice["server"], serde_json::json!(["server-admin"]));
}

// ---------------------------------------------------------------------------

/// Every `.route(…)` of `router()` (and the auth routes) is in the route table, with a
/// need for each method it serves.
#[test]
fn route_coverage() {
    // the routers: `http::router` up to its layers, and the auth routes
    let routers = [
        include_str!("../../../http.rs"),
        include_str!("../../../auth/api.rs"),
        include_str!("../../../auth/handlers.rs"),
    ];
    let mut body = String::new();
    for src in routers {
        let start = src.find("Router::new()").unwrap();
        let rest = &src[start..];
        let end = rest.find(".layer(").or_else(|| rest.find("\n}")).unwrap();
        body.push_str(&rest[..end]);
    }
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
    assert!(found > 45, "{found}");
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

// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_cost_is_bounded() {
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

// ---------------------------------------------------------------------------

#[test]
fn hashes_and_tokens() {
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
            std::path::Path::new("/nonexistent"),
            "127.0.0.1"
        )
        .is_err()
    );
}
