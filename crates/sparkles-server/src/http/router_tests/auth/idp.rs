//! Credentials of the identity provider: OIDC access tokens on the API, back-channel
//! logout, Cloudflare Access assertions, and the groups they refresh in the tokens and
//! sessions of their owners. Also idle sessions and device grants across a restart.

use super::oidc::{Mock, PUBLIC, full_login, jwk_of, oidc_server_with, other_key, start_mock};
use super::*;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;

const API_AUD: &str = "https://sparql.example.org/api";
const API: &str = r#"api_audience = "https://sparql.example.org/api"
api_scopes = ["sparkles"]"#;

fn now() -> i64 {
    jsonwebtoken::get_current_timestamp() as i64
}

fn with(mut c: J, k: &str, v: J) -> J {
    c[k] = v;
    c
}

fn without(mut c: J, k: &str) -> J {
    c.as_object_mut().unwrap().remove(k);
    c
}

/// The claims of a good access token for alice.
fn at_claims(m: &Mock) -> J {
    json!({
        "iss": m.issuer,
        "sub": "u-1",
        "aud": API_AUD,
        "iat": now(),
        "nbf": now(),
        "exp": now() + 300,
        "scope": "openid sparkles",
        "email": "alice@example.org",
        "groups": ["sparkles", "kg-editors"],
    })
}

async fn whoami(s: &AuthServer, token: &str) -> R {
    get_as(&s.app, "/$/whoami", Some(&bearer(token))).await
}

fn b64(v: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v)
}

/// A token with a hand-written header (`alg: none` and the like) and a dummy signature.
fn forged(header: J, claims: &J) -> String {
    format!(
        "{}.{}.{}",
        b64(header.to_string().as_bytes()),
        b64(claims.to_string().as_bytes()),
        b64(b"not-a-signature")
    )
}

#[tokio::test]
async fn access_tokens_authenticate_on_the_api() {
    let (s, m) = oidc_server_with(API).await;
    let t = m.sign(&at_claims(&m));
    let r = whoami(&s, &t).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let w = r.json();
    assert_eq!(w["principal"]["kind"], "oidc");
    assert_eq!(w["principal"]["name"], "alice@example.org");
    assert_eq!(w["principal"]["groups"], json!(["sparkles", "kg-editors"]));
    assert_eq!(w["method"], "bearer");
    assert_eq!(w["datasets"]["wiki"], "write");
    assert_eq!(w["canMintTokens"], false);
    assert!(w["csrfToken"].is_null(), "a bearer token is not ambient");
    assert!(w["expires"].is_string());
    // a bearer credential needs no CSRF token
    let u = update_as(&s.app, "wiki", &bearer(&t), INSERT).await;
    assert_eq!(u.status, StatusCode::OK, "{}", u.text());
    assert_eq!(
        get_as(&s.app, &format!("/secret{ASK}"), Some(&bearer(&t)))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // the provider's short-lived token cannot mint long-lived Sparkles tokens
    let mint = call(
        &s.app,
        "POST",
        "/$/auth/tokens",
        &[
            ("authorization", &bearer(&t)),
            ("content-type", "application/json"),
        ],
        r#"{"name":"x"}"#,
    )
    .await;
    assert_eq!(mint.status, StatusCode::FORBIDDEN, "{}", mint.text());
    // Sparkles tokens keep working next to them
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&tok('A'))))
            .await
            .status,
        StatusCode::OK
    );
    // client credentials: the account is named by another claim when configured
    let (s2, m2) = oidc_server_with(&format!("{API}\napi_name_claim = \"client_id\"")).await;
    let c = with(
        without(at_claims(&m2), "email"),
        "client_id",
        json!("etl-job"),
    );
    let w = whoami(&s2, &m2.sign(&c)).await.json();
    assert_eq!(w["principal"]["name"], "etl-job");
}

#[tokio::test]
async fn refused_access_tokens() {
    let (s, m) = oidc_server_with(API).await;
    let c = at_claims(&m);
    let other = EncodingKey::from_rsa_der(other_key());
    // an HMAC token keyed with the provider's public key (algorithm confusion)
    let public_key = serde_json::to_vec(&*m.jwks.lock()).unwrap();
    let hs = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &c,
        &EncodingKey::from_secret(&public_key),
    )
    .unwrap();
    let mut crit = Header::new(Algorithm::RS256);
    crit.kid = Some("k1".into());
    crit.crit = Some(vec!["exp".into()]);
    let crit = jsonwebtoken::encode(&crit, &c, &m.key).unwrap();
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a bad signature",
            m.sign_with(&other, "k1", &c),
            "invalid credentials",
        ),
        (
            "another issuer",
            m.sign(&with(c.clone(), "iss", json!("https://evil.example"))),
            "invalid credentials",
        ),
        (
            "an issuer list",
            m.sign(&with(c.clone(), "iss", json!([m.issuer, "https://x"]))),
            "invalid credentials",
        ),
        (
            "the UI client's audience",
            m.sign(&with(c.clone(), "aud", json!("sparkles"))),
            "invalid credentials",
        ),
        (
            "no audience",
            m.sign(&without(c.clone(), "aud")),
            "invalid credentials",
        ),
        (
            "an expired token",
            m.sign(&with(c.clone(), "exp", json!(now() - 120))),
            "token expired",
        ),
        (
            "no expiry",
            m.sign(&without(c.clone(), "exp")),
            "invalid credentials",
        ),
        (
            "a token not valid yet",
            m.sign(&with(c.clone(), "nbf", json!(now() + 600))),
            "invalid credentials",
        ),
        (
            "a token issued in the future",
            m.sign(&with(c.clone(), "iat", json!(now() + 3600))),
            "invalid credentials",
        ),
        (
            "a missing scope",
            m.sign(&with(c.clone(), "scope", json!("openid"))),
            "invalid credentials",
        ),
        (
            "no account claim",
            m.sign(&without(c.clone(), "email")),
            "invalid credentials",
        ),
        (
            "an unknown key id",
            m.sign_with(&m.key, "k9", &c),
            "invalid credentials",
        ),
        (
            "alg none",
            forged(json!({ "alg": "none", "typ": "JWT" }), &c),
            "invalid credentials",
        ),
        (
            "alg none without a signature",
            forged(json!({ "alg": "none" }), &c)
                .rsplit_once('.')
                .map(|(a, _)| format!("{a}."))
                .unwrap(),
            "invalid credentials",
        ),
        ("HS256 keyed with the public key", hs, "invalid credentials"),
        ("a critical header", crit, "invalid credentials"),
        (
            "an embedded key",
            forged(json!({ "alg": "RS256", "jwk": jwk_of(&other, "k1") }), &c),
            "invalid credentials",
        ),
    ];
    for (what, t, msg) in &cases {
        let r = whoami(&s, t).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{what}: {}", r.text());
        assert_eq!(r.json()["error"], *msg, "{what}");
        assert!(
            r.header("www-authenticate").contains("invalid_token"),
            "{what}"
        );
    }
    // an expired token within the clock leeway still works
    let late = m.sign(&with(c.clone(), "exp", json!(now() - 30)));
    assert_eq!(whoami(&s, &late).await.status, StatusCode::OK);
    // an account the provider vouches for but [external] does not admit
    let outsider = m.sign(&with(c.clone(), "groups", json!(["kg-editors"])));
    let r = get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&outsider))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["error"], "user not allowed");
    assert!(
        metric(
            &s.app,
            "sparkles_auth_failures_total{scheme=\"bearer\",reason=\"invalid\"}"
        )
        .await
            >= 10
    );
    assert!(
        metric(
            &s.app,
            "sparkles_auth_failures_total{scheme=\"bearer\",reason=\"expired\"}"
        )
        .await
            >= 1
    );
}

#[tokio::test]
async fn access_tokens_need_an_api_audience() {
    let (s, m) = oidc_server_with("").await;
    // without api_audience a JWT is just an unknown token
    let r = whoami(&s, &m.sign(&at_claims(&m))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // nor is the UI login's ID token an API credential
    let id = with(at_claims(&m), "aud", json!("sparkles"));
    assert_eq!(
        whoami(&s, &m.sign(&id)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn keys_rotate() {
    let (s, m) = oidc_server_with(API).await;
    let c = at_claims(&m);
    assert_eq!(whoami(&s, &m.sign(&c)).await.status, StatusCode::OK);
    // the provider rotates: k2 replaces k1
    let k2 = EncodingKey::from_rsa_der(other_key());
    *m.jwks.lock() = json!({ "keys": [jwk_of(&k2, "k2")] });
    let fetches = m.jwks_fetches.load(Ordering::Relaxed);
    let r = whoami(&s, &m.sign_with(&k2, "k2", &c)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(m.jwks_fetches.load(Ordering::Relaxed), fetches + 1);
    // the retired key no longer verifies
    assert_eq!(
        whoami(&s, &m.sign(&c)).await.status,
        StatusCode::UNAUTHORIZED
    );
    // unknown key ids do not make the server fetch the set again and again
    let fetches = m.jwks_fetches.load(Ordering::Relaxed);
    for i in 0..5 {
        let t = m.sign_with(&k2, &format!("random-{i}"), &c);
        assert_eq!(whoami(&s, &t).await.status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(m.jwks_fetches.load(Ordering::Relaxed), fetches);
}

#[tokio::test]
async fn provider_keys_unavailable() {
    let (s, m) = oidc_server_with(API).await;
    m.behavior.lock().jwks_down = true;
    let t = m.sign(&at_claims(&m));
    let r = whoami(&s, &t).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["error"], "identity provider unavailable");
    assert!(!r.header("retry-after").is_empty());
    m.behavior.lock().jwks_down = false;
    assert_eq!(whoami(&s, &t).await.status, StatusCode::OK);
    // once fetched, the keys outlive a provider outage
    m.behavior.lock().jwks_down = true;
    assert_eq!(whoami(&s, &t).await.status, StatusCode::OK);
}

// ---------------------------------------------------------- back-channel logout ------

fn logout_claims(m: &Mock, sid: Option<&str>) -> J {
    let mut c = json!({
        "iss": m.issuer,
        "aud": "sparkles",
        "iat": now(),
        "exp": now() + 120,
        "jti": crate::auth::crypto::random_token(16),
        "sub": "u-1",
    });
    let mut events = serde_json::Map::new();
    events.insert(crate::auth::oidc::BACKCHANNEL_EVENT.into(), json!({}));
    c["events"] = J::Object(events);
    if let Some(sid) = sid {
        c["sid"] = sid.into();
    }
    c
}

async fn backchannel(s: &AuthServer, token: &str) -> R {
    call(
        &s.app,
        "POST",
        "/$/auth/oidc/backchannel-logout",
        &[("content-type", "application/x-www-form-urlencoded")],
        &format!("logout_token={token}"),
    )
    .await
}

async fn session_kind(s: &AuthServer, cookie: &str) -> J {
    call(&s.app, "GET", "/$/whoami", &[("cookie", cookie)], "")
        .await
        .json()["principal"]["kind"]
        .clone()
}

#[tokio::test]
async fn backchannel_logout_ends_sessions() {
    let (s, m) = oidc_server_with("").await;
    m.behavior
        .lock()
        .claims
        .insert("sid".into(), "sid-1".into());
    let (_, c1) = full_login(&s).await;
    m.behavior
        .lock()
        .claims
        .insert("sid".into(), "sid-2".into());
    let (_, c2) = full_login(&s).await;
    assert_eq!(session_kind(&s, &c1).await, "oidc");
    let good = logout_claims(&m, Some("sid-1"));
    let other = EncodingKey::from_rsa_der(other_key());
    let refused = [
        ("a bad signature", m.sign_with(&other, "k1", &good)),
        (
            "another audience",
            m.sign(&with(good.clone(), "aud", json!("someone-else"))),
        ),
        (
            "another issuer",
            m.sign(&with(good.clone(), "iss", json!("https://evil.example"))),
        ),
        (
            "a nonce (an ID token)",
            m.sign(&with(good.clone(), "nonce", json!("n"))),
        ),
        ("no logout event", m.sign(&without(good.clone(), "events"))),
        (
            "neither sid nor sub",
            m.sign(&without(without(good.clone(), "sid"), "sub")),
        ),
        ("no jti", m.sign(&without(good.clone(), "jti"))),
        ("no iat", m.sign(&without(good.clone(), "iat"))),
        (
            "an old token",
            m.sign(&with(good.clone(), "iat", json!(now() - 3600))),
        ),
        (
            "an expired token",
            m.sign(&with(good.clone(), "exp", json!(now() - 3600))),
        ),
        ("alg none", forged(json!({ "alg": "none" }), &good)),
        ("not a JWT", "garbage".to_string()),
    ];
    for (what, t) in &refused {
        let r = backchannel(&s, t).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}: {}", r.text());
        assert_eq!(r.json()["error"], "invalid_request", "{what}");
    }
    let missing = call(
        &s.app,
        "POST",
        "/$/auth/oidc/backchannel-logout",
        &[("content-type", "application/x-www-form-urlencoded")],
        "",
    )
    .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        session_kind(&s, &c1).await,
        "oidc",
        "a refused logout ended a session"
    );

    // by sid: that session only
    let t = m.sign(&good);
    let r = backchannel(&s, &t).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("cache-control"), "no-store");
    assert_eq!(session_kind(&s, &c1).await, "anonymous");
    assert_eq!(session_kind(&s, &c2).await, "oidc");
    // a replay is refused
    assert_eq!(backchannel(&s, &t).await.status, StatusCode::BAD_REQUEST);
    // by sub: every session of the account
    let r = backchannel(&s, &m.sign(&logout_claims(&m, None))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(session_kind(&s, &c2).await, "anonymous");
}

// ------------------------------------------------------------- Cloudflare Access ------

fn cf_config(m: &Mock) -> String {
    format!(
        "[cloudflare_access]\nteam_domain = \"{}\"\naudience = \"aud-tag\"\ngroups_claim = \"groups\"\n",
        m.issuer
    )
}

fn cf_claims(m: &Mock) -> J {
    json!({
        "iss": m.issuer,
        "aud": ["aud-tag"],
        "sub": "u-9",
        "email": "erin@example.org",
        "iat": now(),
        "nbf": now(),
        "exp": now() + 300,
        "type": "app",
        "groups": ["sparkles", "kg-editors"],
    })
}

async fn cf_call(s: &AuthServer, method: &str, uri: &str, t: &str, more: &[(&str, &str)]) -> R {
    let mut h = vec![("cf-access-jwt-assertion", t)];
    h.extend_from_slice(more);
    call(
        &s.app,
        method,
        uri,
        &h,
        if method == "POST" { INSERT } else { "" },
    )
    .await
}

#[tokio::test]
async fn cloudflare_access_assertions() {
    let m = start_mock().await;
    let s = build(Fixture {
        extra: cf_config(&m),
        ..Default::default()
    });
    let t = m.sign(&cf_claims(&m));
    // from any peer: the signature is what counts
    let w = cf_call(&s, "GET", "/$/whoami", &t, &[]).await;
    assert_eq!(w.status, StatusCode::OK, "{}", w.text());
    let w = w.json();
    assert_eq!(w["principal"]["kind"], "proxy");
    assert_eq!(w["principal"]["name"], "erin@example.org");
    assert_eq!(w["method"], "proxy");
    assert_eq!(w["datasets"]["wiki"], "write");
    assert_eq!(w["canMintTokens"], true);
    assert_eq!(w["logout"], true);
    // the edge adds it to every browser request: ambient, so CSRF applies
    let csrf = w["csrfToken"].as_str().unwrap().to_string();
    let ct = ("content-type", "application/sparql-update");
    let no = cf_call(&s, "POST", "/wiki/update", &t, &[ct]).await;
    assert_eq!(no.status, StatusCode::FORBIDDEN);
    let lo = cf_call(
        &s,
        "POST",
        "/$/auth/logout",
        &t,
        &[("x-sparkles-csrf", &csrf)],
    )
    .await;
    assert_eq!(lo.json()["redirect"], "/cdn-cgi/access/logout");
    let yes = cf_call(
        &s,
        "POST",
        "/wiki/update",
        &t,
        &[ct, ("x-sparkles-csrf", &csrf)],
    )
    .await;
    assert_eq!(yes.status, StatusCode::OK, "{}", yes.text());

    // a service token: not ambient, cannot mint
    let svc = with(
        without(cf_claims(&m), "email"),
        "common_name",
        json!("abc123.access"),
    );
    let st = m.sign(&svc);
    let w = cf_call(&s, "GET", "/$/whoami", &st, &[]).await.json();
    assert_eq!(w["principal"]["name"], "abc123.access");
    assert_eq!(w["method"], "bearer");
    assert!(w["csrfToken"].is_null());
    assert_eq!(w["canMintTokens"], false);
    let u = cf_call(&s, "POST", "/wiki/update", &st, &[ct]).await;
    assert_eq!(u.status, StatusCode::OK, "{}", u.text());

    let c = cf_claims(&m);
    let other = EncodingKey::from_rsa_der(other_key());
    for (what, bad, msg) in [
        (
            "a bad signature",
            m.sign_with(&other, "k1", &c),
            "invalid credentials",
        ),
        (
            "another application",
            m.sign(&with(c.clone(), "aud", json!(["other-app"]))),
            "invalid credentials",
        ),
        (
            "another team",
            m.sign(&with(
                c.clone(),
                "iss",
                json!("https://evil.cloudflareaccess.com"),
            )),
            "invalid credentials",
        ),
        (
            "an expired assertion",
            m.sign(&with(c.clone(), "exp", json!(now() - 600))),
            "token expired",
        ),
        (
            "alg none",
            forged(json!({ "alg": "none" }), &c),
            "invalid credentials",
        ),
        (
            "no account",
            m.sign(&without(c.clone(), "email")),
            "invalid credentials",
        ),
    ] {
        let r = cf_call(&s, "GET", &format!("/public{ASK}"), &bad, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{what}: {}", r.text());
        assert_eq!(r.json()["error"], msg, "{what}");
    }
    // vouched for by Access but not admitted
    let outsider = m.sign(&with(c.clone(), "groups", json!(["other"])));
    let r = cf_call(&s, "GET", &format!("/wiki{ASK}"), &outsider, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Authorization wins over the assertion
    let r = call(
        &s.app,
        "GET",
        "/$/whoami",
        &[
            ("cf-access-jwt-assertion", &t),
            ("authorization", &b("bob")),
        ],
        "",
    )
    .await;
    assert_eq!(r.json()["principal"]["name"], "bob");
}

#[test]
fn cloudflare_access_settings() {
    let base = config_text(&Fixture::default());
    let parse = |extra: &str| crate::auth::config::FileConfig::parse(&format!("{base}\n{extra}"));
    parse("[cloudflare_access]\nteam_domain = \"https://team.cloudflareaccess.com\"\naudience = [\"a\", \"b\"]\n")
        .unwrap();
    for (bad, why) in [
        (
            "[cloudflare_access]\nteam_domain = \"http://team.example.com\"\naudience = \"a\"\n",
            "team_domain",
        ),
        (
            "[cloudflare_access]\nteam_domain = \"https://team.cloudflareaccess.com/x\"\naudience = \"a\"\n",
            "team_domain",
        ),
        (
            "[cloudflare_access]\nteam_domain = \"https://team.cloudflareaccess.com\"\naudience = []\n",
            "audience",
        ),
        (
            "[cloudflare_access]\nteam_domain = \"https://team.cloudflareaccess.com\"\naudience = \"a\"\n[proxy]\npreset = \"cloudflare-access\"\ntrusted = [\"127.0.0.1\"]\n",
            "preset",
        ),
    ] {
        let e = parse(bad).unwrap_err().to_string();
        assert!(e.contains(why), "{bad}: {e}");
    }
    let oidc = "[oidc]\nissuer = \"https://idp.example.org\"\nclient_id = \"x\"\n";
    let e = parse(&format!("{oidc}api_scopes = [\"a\"]\n"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("api_audience"), "{e}");
    let e = parse(&format!(
        "{oidc}api_audience = \"a\"\napi_name_claim = \"name\"\n"
    ))
    .unwrap_err()
    .to_string();
    assert!(e.contains("api_name_claim"), "{e}");
    let cfg = parse(&format!("{oidc}api_audience = [\"x\", \"api\"]\n")).unwrap();
    assert!(
        cfg.warnings().iter().any(|w| w.contains("api_audience")),
        "{:?}",
        cfg.warnings()
    );
}

// --------------------------------------------------------------- groups refresh ------

/// Mint a token from a session or proxy principal; returns it.
async fn mint_with(s: &AuthServer, peer: Peer, headers: &[(&str, &str)]) -> String {
    let mut h = headers.to_vec();
    h.push(("content-type", "application/json"));
    let r = call_from(
        &s.app,
        peer,
        "POST",
        "/$/auth/tokens",
        &h,
        r#"{"name":"ci","datasets":{"wiki":"write"}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn oidc_token_owners_follow_their_groups() {
    let (s, m) = oidc_server_with("").await;
    let (_, cookie) = full_login(&s).await;
    let csrf = csrf_of(&s.app, &cookie).await;
    let t = mint_with(
        &s,
        default_peer(),
        &[
            ("cookie", &cookie),
            ("x-sparkles-csrf", &csrf),
            ("origin", PUBLIC),
        ],
    )
    .await;
    assert_eq!(
        update_as(&s.app, "wiki", &bearer(&t), INSERT).await.status,
        StatusCode::OK
    );
    // the provider drops kg-editors: the next login records it in the token
    m.behavior
        .lock()
        .claims
        .insert("groups".into(), json!(["sparkles"]));
    let (r, _) = full_login(&s).await;
    assert_eq!(r.header("location"), "/ui/datasets");
    let u = update_as(&s.app, "wiki", &bearer(&t), INSERT).await;
    assert_eq!(u.status, StatusCode::NOT_FOUND, "{}", u.text());
    let file = std::fs::read_to_string(s.dir.path().join("auth/tokens.json")).unwrap();
    let rec: J = serde_json::from_str(&file).unwrap();
    assert_eq!(rec["tokens"][0]["owner"]["groups"], json!(["sparkles"]));
    // the older session follows too
    assert!(
        call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
            .await
            .json()["datasets"]["wiki"]
            .is_null()
    );
    // a login that is no longer admitted still updates the token, which then fails
    m.behavior
        .lock()
        .claims
        .insert("groups".into(), json!(["other"]));
    let (r, _) = full_login(&s).await;
    assert_eq!(r.header("location"), "/ui/login?error=not_allowed");
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&bearer(&t)))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn proxy_token_owners_follow_their_groups() {
    let s = build(Fixture {
        extra: "[proxy]\npreset = \"authelia\"\ntrusted = [\"127.0.0.1/32\"]\n".into(),
        ..Default::default()
    });
    let proxy = Peer::Tcp("127.0.0.1:40000".parse().unwrap());
    let dave = |groups: &'static str| [("remote-user", "dave"), ("remote-groups", groups)];
    let w = call_from(
        &s.app,
        proxy,
        "GET",
        "/$/whoami",
        &dave("sparkles,kg-editors"),
        "",
    )
    .await
    .json();
    let csrf = w["csrfToken"].as_str().unwrap().to_string();
    let mut h = dave("sparkles,kg-editors").to_vec();
    h.push(("x-sparkles-csrf", &csrf));
    let t = mint_with(&s, proxy, &h).await;
    let write = || async { update_as(&s.app, "wiki", &bearer(&t), INSERT).await.status };
    assert_eq!(write().await, StatusCode::OK);
    call_from(&s.app, proxy, "GET", "/$/whoami", &dave("sparkles"), "").await;
    assert_eq!(write().await, StatusCode::NOT_FOUND);
    call_from(
        &s.app,
        proxy,
        "GET",
        "/$/whoami",
        &dave("sparkles,kg-editors"),
        "",
    )
    .await;
    assert_eq!(write().await, StatusCode::OK);
    // a request without the groups header asserts nothing about them
    call_from(
        &s.app,
        proxy,
        "GET",
        "/$/whoami",
        &[("remote-user", "dave")],
        "",
    )
    .await;
    assert_eq!(write().await, StatusCode::OK);
}

// ------------------------------------------------------------------ idle sessions ------

#[tokio::test]
async fn idle_sessions_end() {
    let s = build(Fixture {
        extra: "[session]\nttl = \"12h\"\nidle_timeout = \"10m\"\n".into(),
        ..Default::default()
    });
    let (cookie, _) = password_session(&s, "bob").await;
    let who = || async { call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "").await };
    let w = who().await.json();
    assert_eq!(w["principal"]["kind"], "user");
    let expires = crate::auth::config::parse_rfc3339(w["expires"].as_str().unwrap()).unwrap();
    assert!((expires - s.auth().now() - 600).abs() <= 2, "{w}");
    // each use keeps it alive
    for _ in 0..3 {
        s.auth().advance(540);
        assert_eq!(who().await.json()["principal"]["kind"], "user");
    }
    s.auth().advance(601);
    let r = who().await;
    assert_eq!(r.json()["principal"]["kind"], "anonymous");
    assert!(
        r.set_cookie("sparkles_session")
            .unwrap()
            .contains("Max-Age=0")
    );
}

#[test]
fn idle_timeout_within_ttl() {
    let base = config_text(&Fixture::default());
    let e = crate::auth::config::FileConfig::parse(&format!(
        "{base}\n[session]\nttl = \"1h\"\nidle_timeout = \"2h\"\n"
    ))
    .unwrap_err();
    assert!(e.to_string().contains("idle_timeout"), "{e}");
}

// --------------------------------------------------------- device grants, restart ------

async fn device_poll(s: &AuthServer, device_code: &str) -> R {
    super::cli_grants::form(
        &s.app,
        "/$/auth/token",
        &format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={device_code}"
        ),
    )
    .await
}

#[tokio::test]
async fn device_grants_survive_restarts() {
    let s = auth_server();
    let g = super::cli_grants::form(&s.app, "/$/auth/device", "label=cli&hostname=h")
        .await
        .json();
    let (dc, uc) = (
        g["device_code"].as_str().unwrap().to_string(),
        g["user_code"].as_str().unwrap().to_string(),
    );
    // pending across a restart
    let s = s.restart();
    let (cookie, csrf) = password_session(&s, "alice").await;
    let ok = call(
        &s.app,
        "POST",
        &format!("/$/auth/device/{uc}/approve"),
        &[
            ("cookie", &cookie),
            ("x-sparkles-csrf", &csrf),
            ("content-type", "application/json"),
        ],
        r#"{"name":"laptop","expiresIn":"7d"}"#,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    // approved, then a restart before the CLI polled: the token gets a new secret
    let s = s.restart();
    let t = device_poll(&s, &dc).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.text());
    let tj = t.json();
    assert_eq!(tj["principal"], "user:alice");
    let exp = tj["expires_in"].as_i64().unwrap();
    assert!((7 * 86400 - 60..=7 * 86400).contains(&exp), "{tj}");
    let token = tj["access_token"].as_str().unwrap();
    let w = get_as(&s.app, "/$/whoami", Some(&bearer(token)))
        .await
        .json();
    assert_eq!(w["principal"]["owner"], "user:alice");
    assert_eq!(device_poll(&s, &dc).await.json()["error"], "expired_token");
    for f in ["tokens.json", "device-grants.json"] {
        let text = std::fs::read_to_string(s.dir.path().join("auth").join(f)).unwrap();
        assert!(!text.contains("spk_"), "{f}: {text}");
        assert!(!text.contains(&dc), "{f}: {text}");
    }
    // a grant approved and revoked before the restart gives nothing
    let g = super::cli_grants::form(&s.app, "/$/auth/device", "label=cli")
        .await
        .json();
    let (dc, uc) = (
        g["device_code"].as_str().unwrap().to_string(),
        g["user_code"].as_str().unwrap().to_string(),
    );
    let (cookie, csrf) = password_session(&s, "alice").await;
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
    .await
    .json();
    let id = ok["tokenId"].as_str().unwrap();
    s.auth().tokens.remove(&[id.to_string()]).unwrap();
    let s = s.restart();
    assert_eq!(device_poll(&s, &dc).await.json()["error"], "expired_token");
}
