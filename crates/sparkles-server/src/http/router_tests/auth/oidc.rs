//! OIDC login against an in-process mock provider: discovery, JWKS with an RS256 key,
//! an authorize endpoint that answers at once, a token endpoint that checks PKCE and
//! the client secret, UserInfo and an end-session URL.

use super::*;
use axum::extract::{Query, State as AxState};
use axum::response::{IntoResponse, Redirect};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use std::collections::HashMap;
use std::sync::OnceLock;

const PUBLIC: &str = "https://sparql.example.org";
const SECRET: &str = "s3cret";

/// How the mock provider misbehaves, and the claims it issues.
#[derive(Clone, Default)]
struct Behavior {
    claims: serde_json::Map<String, J>,
    userinfo: serde_json::Map<String, J>,
    wrong_nonce: bool,
    wrong_aud: bool,
    expired: bool,
}

struct Mock {
    issuer: String,
    key: EncodingKey,
    jwks: J,
    codes: parking_lot::Mutex<HashMap<String, (String, String)>>,
    behavior: parking_lot::Mutex<Behavior>,
}

/// A PKCS#1 `RSAPrivateKey` from a PKCS#8 `PrivateKeyInfo` (DER).
fn pkcs1_of(pkcs8: &[u8]) -> Vec<u8> {
    fn tlv(b: &[u8]) -> (u8, &[u8], &[u8]) {
        let (len, hdr) = if b[1] < 0x80 {
            (b[1] as usize, 2)
        } else {
            let n = (b[1] & 0x7f) as usize;
            let len = b[2..2 + n]
                .iter()
                .fold(0usize, |l, x| (l << 8) | *x as usize);
            (len, 2 + n)
        };
        (b[0], &b[hdr..hdr + len], &b[hdr + len..])
    }
    let (_, seq, _) = tlv(pkcs8);
    let (_, _, rest) = tlv(seq); // version
    let (_, _, rest) = tlv(rest); // algorithm
    let (tag, key, _) = tlv(rest);
    assert_eq!(tag, 0x04);
    key.to_vec()
}

/// One RSA key per test binary.
fn test_key() -> &'static Vec<u8> {
    static KEY: OnceLock<Vec<u8>> = OnceLock::new();
    KEY.get_or_init(|| {
        use aws_lc_rs::encoding::AsDer;
        let k = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
        pkcs1_of(k.as_der().unwrap().as_ref())
    })
}

async fn discovery(AxState(m): AxState<Arc<Mock>>) -> impl IntoResponse {
    let i = &m.issuer;
    axum::Json(serde_json::json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "jwks_uri": format!("{i}/jwks"),
        "userinfo_endpoint": format!("{i}/userinfo"),
        "end_session_endpoint": format!("{i}/logout"),
        "code_challenge_methods_supported": ["S256"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
    }))
}

async fn authorize(
    AxState(m): AxState<Arc<Mock>>,
    Query(q): Query<HashMap<String, String>>,
) -> Redirect {
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256");
    let code = crate::auth::crypto::random_token(16);
    m.codes.lock().insert(
        code.clone(),
        (q["nonce"].clone(), q["code_challenge"].clone()),
    );
    let state: String = form_urlencoded::byte_serialize(q["state"].as_bytes()).collect();
    Redirect::to(&format!("{}?code={code}&state={state}", q["redirect_uri"]))
}

async fn token(
    AxState(m): AxState<Arc<Mock>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let f: HashMap<String, String> = form_urlencoded::parse(&body).into_owned().collect();
    let want = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("sparkles:{SECRET}"))
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(want.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    let Some((nonce, challenge)) = m.codes.lock().remove(&f["code"]) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    if crate::auth::crypto::pkce_challenge(&f["code_verifier"]) != challenge {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    let b = m.behavior.lock().clone();
    let now = jsonwebtoken::get_current_timestamp() as i64;
    let mut claims = serde_json::json!({
        "iss": m.issuer,
        "sub": "u-1",
        "aud": if b.wrong_aud { "someone-else" } else { "sparkles" },
        "iat": now,
        "exp": if b.expired { now - 3600 } else { now + 300 },
        "nonce": if b.wrong_nonce { "nope".to_string() } else { nonce },
    });
    for (k, v) in b.claims {
        claims[k] = v;
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("k1".into());
    let id_token = jsonwebtoken::encode(&header, &claims, &m.key).unwrap();
    axum::Json(serde_json::json!({
        "id_token": id_token,
        "access_token": "at-1",
        "token_type": "Bearer",
    }))
    .into_response()
}

async fn jwks_endpoint(AxState(m): AxState<Arc<Mock>>) -> impl IntoResponse {
    axum::Json(m.jwks.clone())
}

async fn userinfo(AxState(m): AxState<Arc<Mock>>) -> impl IntoResponse {
    let mut u = m.behavior.lock().userinfo.clone();
    u.insert("sub".into(), "u-1".into());
    axum::Json(J::Object(u))
}

/// Start the mock provider; returns it and its issuer URL.
async fn start_mock() -> Arc<Mock> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let key = EncodingKey::from_rsa_der(test_key());
    let mut jwk = jsonwebtoken::jwk::Jwk::from_encoding_key(&key, Algorithm::RS256).unwrap();
    jwk.common.key_id = Some("k1".into());
    let jwks = serde_json::json!({ "keys": [jwk] });
    let mut behavior = Behavior::default();
    behavior
        .claims
        .insert("email".into(), "alice@example.org".into());
    behavior.claims.insert("name".into(), "Alice".into());
    behavior.claims.insert(
        "groups".into(),
        serde_json::json!(["sparkles", "kg-editors"]),
    );
    let m = Arc::new(Mock {
        issuer,
        key,
        jwks,
        codes: Default::default(),
        behavior: parking_lot::Mutex::new(behavior),
    });
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(discovery),
        )
        .route("/authorize", axum::routing::get(authorize))
        .route("/token", axum::routing::post(token))
        .route("/jwks", axum::routing::get(jwks_endpoint))
        .route("/userinfo", axum::routing::get(userinfo))
        .with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    m
}

async fn oidc_server() -> (AuthServer, Arc<Mock>) {
    let m = start_mock().await;
    let dir_secret = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(dir_secret.path(), format!("{SECRET}\n")).unwrap();
    let (_, secret_path) = dir_secret.keep().unwrap();
    let s = build(Fixture {
        public_url: PUBLIC,
        extra: format!(
            r#"
[oidc]
issuer = "{}"
client_id = "sparkles"
client_secret_file = "{}"
scopes = ["openid", "email", "groups"]
display_name = "Mock SSO"
algorithms = ["RS256"]
"#,
            m.issuer,
            secret_path.display()
        ),
        ..Default::default()
    });
    (s, m)
}

fn query_of(url: &str) -> HashMap<String, String> {
    let q = url.split_once('?').map_or("", |x| x.1);
    form_urlencoded::parse(q.as_bytes()).into_owned().collect()
}

/// Start a login; returns the provider URL and the login cookie (`name=value`).
async fn begin(s: &AuthServer, return_to: &str) -> (String, String, R) {
    let r = call(
        &s.app,
        "GET",
        &format!("/$/auth/oidc/login?return_to={return_to}"),
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FOUND, "{}", r.text());
    let cookie = r.cookie("sparkles_oidc").unwrap();
    (r.header("location"), cookie, r)
}

/// Let the provider answer: the callback path and query it redirects to.
async fn provider(url: &str) -> String {
    let c = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = c.get(url).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 303, "{:?}", r.text().await);
    let loc = r.headers()["location"].to_str().unwrap().to_string();
    loc.strip_prefix(PUBLIC).unwrap().to_string()
}

async fn callback(s: &AuthServer, path: &str, cookie: &str) -> R {
    call(&s.app, "GET", path, &[("cookie", cookie)], "").await
}

/// A full login; returns the session cookie.
async fn full_login(s: &AuthServer) -> (R, String) {
    let (url, cookie, _) = begin(s, "/ui/datasets").await;
    let path = provider(&url).await;
    let r = callback(s, &path, &cookie).await;
    let session = r.cookie("sparkles_session").unwrap_or_default();
    (r, session)
}

#[tokio::test]
async fn login_redirects_to_the_provider() {
    let (s, m) = oidc_server().await;
    let (url, _, r) = begin(&s, "/ui/datasets").await;
    assert!(
        url.starts_with(&format!("{}/authorize?", m.issuer)),
        "{url}"
    );
    let q = query_of(&url);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "sparkles");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["code_challenge"].len(), 43);
    assert!(!q["state"].is_empty() && !q["nonce"].is_empty());
    assert_eq!(q["redirect_uri"], format!("{PUBLIC}/$/auth/oidc/callback"));
    assert_eq!(q["scope"], "openid email groups");
    let set = r.set_cookie("sparkles_oidc").unwrap();
    assert!(set.starts_with("__Host-sparkles_oidc="), "{set}");
    for attr in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=600",
    ] {
        assert!(set.contains(attr), "{set}");
    }
    let config = get_as(&s.app, "/$/auth/config", None).await.json();
    assert_eq!(config["oidc"]["displayName"], "Mock SSO");
    assert!(
        config["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "oidc")
    );
}

#[tokio::test]
async fn callback_starts_a_session() {
    let (s, _m) = oidc_server().await;
    let (r, cookie) = full_login(&s).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(r.header("location"), "/ui/datasets");
    let set = r.set_cookie("sparkles_session").unwrap();
    assert!(set.starts_with("__Host-sparkles_session="), "{set}");
    for attr in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=43200",
    ] {
        assert!(set.contains(attr), "{set}");
    }
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["principal"]["kind"], "oidc");
    assert_eq!(who["principal"]["name"], "alice@example.org");
    assert_eq!(who["principal"]["displayName"], "Alice");
    assert_eq!(
        who["principal"]["groups"],
        serde_json::json!(["sparkles", "kg-editors"])
    );
    assert_eq!(who["method"], "session");
    assert_eq!(who["datasets"]["wiki"], "write");
    assert!(who["csrfToken"].is_string());
    let raw = cookie.split_once('=').unwrap().1;
    let file = std::fs::read_to_string(s.dir.path().join("auth/sessions.json")).unwrap();
    assert!(!file.contains(raw.rsplit_once('.').unwrap().0));
    assert_eq!(
        serde_json::from_str::<J>(&file).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // an unsafe return_to becomes /ui/
    let (url, oc, _) = begin(&s, "https://evil.example").await;
    let path = provider(&url).await;
    assert_eq!(callback(&s, &path, &oc).await.header("location"), "/ui/");
}

#[tokio::test]
async fn callback_failures() {
    let (s, m) = oidc_server().await;
    let is_error = |r: &R, code: &str| {
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
        assert_eq!(r.header("location"), format!("/ui/login?error={code}"));
        assert!(
            r.set_cookie("sparkles_session").is_none(),
            "a failed login set a session"
        );
    };
    // a state from another browser's login
    let (url_a, _cookie_a, _) = begin(&s, "/ui/").await;
    let (_url_b, cookie_b, _) = begin(&s, "/ui/").await;
    let path_a = provider(&url_a).await;
    is_error(&callback(&s, &path_a, &cookie_b).await, "state");
    // replay
    let (url, cookie, _) = begin(&s, "/ui/").await;
    let path = provider(&url).await;
    assert_eq!(
        callback(&s, &path, &cookie).await.status,
        StatusCode::SEE_OTHER
    );
    is_error(&callback(&s, &path, &cookie).await, "state");
    // bad ID tokens
    for (field, set) in [("nonce", 0), ("aud", 1), ("exp", 2)] {
        {
            let mut b = m.behavior.lock();
            b.wrong_nonce = set == 0;
            b.wrong_aud = set == 1;
            b.expired = set == 2;
        }
        let (r, _) = full_login(&s).await;
        assert_eq!(
            r.header("location"),
            "/ui/login?error=idp",
            "wrong {field} accepted"
        );
    }
    {
        let mut b = m.behavior.lock();
        b.wrong_nonce = false;
        b.wrong_aud = false;
        b.expired = false;
        b.claims
            .insert("groups".into(), serde_json::json!(["kg-editors"]));
    }
    let (r, _) = full_login(&s).await;
    is_error(&r, "not_allowed");
    assert_eq!(
        metric(
            &s.app,
            "sparkles_auth_logins_total{method=\"oidc\",result=\"denied\"}"
        )
        .await,
        1
    );
    // a provider error
    let (url, cookie, _) = begin(&s, "/ui/").await;
    let state = query_of(&url)["state"].clone();
    let r = callback(
        &s,
        &format!("/$/auth/oidc/callback?error=access_denied&state={state}"),
        &cookie,
    )
    .await;
    is_error(&r, "idp");
}

#[tokio::test]
async fn groups_from_userinfo() {
    let (s, m) = oidc_server().await;
    {
        let mut b = m.behavior.lock();
        b.claims.remove("groups");
        b.userinfo.insert(
            "groups".into(),
            serde_json::json!(["sparkles", "kg-editors"]),
        );
    }
    let (r, cookie) = full_login(&s).await;
    assert_eq!(r.header("location"), "/ui/datasets", "{}", r.text());
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["datasets"]["wiki"], "write");
}

#[tokio::test]
async fn session_csrf_and_logout() {
    let (s, m) = oidc_server().await;
    let (_, cookie) = full_login(&s).await;
    let csrf = csrf_of(&s.app, &cookie).await;
    let no = call(
        &s.app,
        "POST",
        "/wiki/update",
        &[
            ("cookie", &cookie),
            ("content-type", "application/sparql-update"),
        ],
        INSERT,
    )
    .await;
    assert_eq!(no.status, StatusCode::FORBIDDEN);
    let yes = call(
        &s.app,
        "POST",
        "/wiki/update",
        &[
            ("cookie", &cookie),
            ("x-sparkles-csrf", &csrf),
            ("content-type", "application/sparql-update"),
            ("origin", PUBLIC),
        ],
        INSERT,
    )
    .await;
    assert_eq!(yes.status, StatusCode::OK, "{}", yes.text());
    // an OIDC session can approve a CLI login
    let start = super::cli_grants::form(&s.app, "/$/auth/device", "label=cli")
        .await
        .json();
    let uc = start["user_code"].as_str().unwrap();
    let ok = call(
        &s.app,
        "POST",
        &format!("/$/auth/device/{uc}/approve"),
        &[
            ("cookie", &cookie),
            ("x-sparkles-csrf", &csrf),
            ("content-type", "application/json"),
        ],
        "{}",
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    let dc = start["device_code"].as_str().unwrap();
    let t = super::cli_grants::form(
        &s.app,
        "/$/auth/token",
        &format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={dc}"
        ),
    )
    .await
    .json();
    assert_eq!(t["principal"], "oidc:alice@example.org");

    let lo = call(
        &s.app,
        "POST",
        "/$/auth/logout",
        &[("cookie", &cookie), ("x-sparkles-csrf", &csrf)],
        "",
    )
    .await;
    assert_eq!(lo.status, StatusCode::OK, "{}", lo.text());
    let redirect = lo.json()["redirect"].as_str().unwrap().to_string();
    assert!(
        redirect.starts_with(&format!("{}/logout?", m.issuer)),
        "{redirect}"
    );
    let q = query_of(&redirect);
    assert!(q["id_token_hint"].split('.').count() == 3);
    assert_eq!(q["post_logout_redirect_uri"], format!("{PUBLIC}/ui/"));
    assert!(
        lo.set_cookie("sparkles_session")
            .unwrap()
            .contains("Max-Age=0")
    );
    let after = call(
        &s.app,
        "GET",
        &format!("/wiki{ASK}"),
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn oidc_needs_a_public_url() {
    let base = config_text(&Fixture::default());
    let no_public = base.replace("public_url = \"http://localhost:3030\"", "");
    let e = crate::auth::config::FileConfig::parse(&format!(
        "{no_public}\n[oidc]\nissuer = \"https://idp.example.org\"\nclient_id = \"x\"\n"
    ))
    .unwrap_err();
    assert!(e.to_string().contains("public_url"), "{e}");
    let e = crate::auth::config::FileConfig::parse(&format!(
        "{base}\n[oidc]\nissuer = \"https://idp.example.org\"\nclient_id = \"x\"\nalgorithms = [\"HS256\"]\n"
    ))
    .unwrap_err();
    assert!(e.to_string().contains("HS256"), "{e}");
}
