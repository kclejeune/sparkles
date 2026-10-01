//! CLI logins: the device flow (RFC 8628) and the loopback flow with PKCE.

use super::*;
use crate::auth::crypto::pkce_challenge;

pub(super) async fn form(app: &Router, path: &str, body: &str) -> R {
    call(
        app,
        "POST",
        path,
        &[("content-type", "application/x-www-form-urlencoded")],
        body,
    )
    .await
}

async fn poll(app: &Router, device_code: &str) -> R {
    form(
        app,
        "/$/auth/token",
        &format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={device_code}"),
    )
    .await
}

/// `(cookie, csrf)` headers of a session, plus JSON.
fn session_json<'a>(cookie: &'a str, csrf: &'a str) -> [(&'static str, &'a str); 3] {
    [
        ("cookie", cookie),
        ("x-sparkles-csrf", csrf),
        ("content-type", "application/json"),
    ]
}

#[tokio::test]
async fn device_flow() {
    let s = auth_server();
    let start = form(&s.app, "/$/auth/device", "label=sparkles%20CLI&hostname=h").await;
    assert_eq!(start.status, StatusCode::OK, "{}", start.text());
    let g = start.json();
    let device_code = g["device_code"].as_str().unwrap().to_string();
    let user_code = g["user_code"].as_str().unwrap().to_string();
    assert!(
        regex::Regex::new("^[0-9a-f]{64}$")
            .unwrap()
            .is_match(&device_code)
    );
    assert!(
        regex::Regex::new("^[A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}$")
            .unwrap()
            .is_match(&user_code)
    );
    assert_eq!(g["expires_in"], 600);
    assert_eq!(g["interval"], 5);
    assert_eq!(g["verification_uri"], "http://localhost:3030/ui/cli/device");
    assert_eq!(
        g["verification_uri_complete"],
        format!("http://localhost:3030/ui/cli/device?code={user_code}")
    );

    let p = poll(&s.app, &device_code).await;
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
    assert_eq!(p.json()["error"], "authorization_pending");
    assert_eq!(
        poll(&s.app, &device_code).await.json()["error"],
        "slow_down"
    );

    let (cookie, csrf) = password_session(&s, "alice").await;
    let info = call(
        &s.app,
        "GET",
        &format!("/$/auth/device/{}", user_code.to_lowercase()),
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.text());
    assert_eq!(info.json()["label"], "sparkles CLI");
    assert_eq!(info.json()["hostname"], "h");
    assert_eq!(info.json()["status"], "pending");
    let ok = call(
        &s.app,
        "POST",
        &format!("/$/auth/device/{user_code}/approve"),
        &session_json(&cookie, &csrf),
        r#"{"name":"laptop","datasets":{"*":"admin"},"server":["*"],"expiresIn":"30d"}"#,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());

    s.auth().advance(10);
    let t = poll(&s.app, &device_code).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.text());
    assert_eq!(t.header("cache-control"), "no-store");
    let tj = t.json();
    assert_eq!(tj["token_type"], "Bearer");
    assert_eq!(tj["principal"], "user:alice");
    let token = tj["access_token"].as_str().unwrap().to_string();
    assert_eq!(
        poll(&s.app, &device_code).await.json()["error"],
        "expired_token"
    );
    // the token works and lists as a device token
    let who = get_as(&s.app, "/$/whoami", Some(&bearer(&token)))
        .await
        .json();
    assert_eq!(who["server"], serde_json::json!(["server-admin"]));
    let l = get_as(&s.app, "/$/auth/tokens", Some(&bearer(&token)))
        .await
        .json();
    let t0 = &l["tokens"][0];
    assert_eq!(t0["via"], "cli-device");
    assert_eq!(t0["client"]["hostname"], "h");
}

#[tokio::test]
async fn device_refusals() {
    let s = auth_server();
    let (cookie, csrf) = password_session(&s, "alice").await;
    let start = form(&s.app, "/$/auth/device", "").await.json();
    let (dc, uc) = (
        start["device_code"].as_str().unwrap().to_string(),
        start["user_code"].as_str().unwrap().to_string(),
    );
    // a Bearer or Basic principal cannot approve
    let bearer_try = call(
        &s.app,
        "POST",
        &format!("/$/auth/device/{uc}/approve"),
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
        ],
        "{}",
    )
    .await;
    assert_eq!(bearer_try.status, StatusCode::FORBIDDEN);
    assert_eq!(
        bearer_try.json()["error"],
        "this action requires signing in to the web UI"
    );
    let anon = call(&s.app, "GET", &format!("/$/auth/device/{uc}"), &[], "").await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let deny = call(
        &s.app,
        "POST",
        &format!("/$/auth/device/{uc}/deny"),
        &session_json(&cookie, &csrf),
        "",
    )
    .await;
    assert_eq!(deny.status, StatusCode::OK, "{}", deny.text());
    assert_eq!(poll(&s.app, &dc).await.json()["error"], "access_denied");

    // expiry
    let start = form(&s.app, "/$/auth/device", "").await.json();
    let dc = start["device_code"].as_str().unwrap().to_string();
    s.auth().advance(601);
    assert_eq!(poll(&s.app, &dc).await.json()["error"], "expired_token");

    // unknown codes: 404, then 429 after 20 failures
    for i in 0..20 {
        let r = call(
            &s.app,
            "GET",
            "/$/auth/device/AAAA-AAAA",
            &[("cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "lookup {i}");
    }
    let r = call(
        &s.app,
        "GET",
        "/$/auth/device/AAAA-AAAA",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // per owner, not per session: another session of alice's has none left either
    let (other, _) = password_session(&s, "alice").await;
    let r = call(
        &s.app,
        "GET",
        "/$/auth/device/AAAA-AAAA",
        &[("cookie", &other)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // and per client network: carol, from the same address, neither
    let (carol, _) = password_session(&s, "carol").await;
    let r = call(
        &s.app,
        "GET",
        "/$/auth/device/AAAA-AAAA",
        &[("cookie", &carol)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // from another address, carol still can
    let r = call_from(
        &s.app,
        Peer::Tcp("198.51.100.3:1".parse().unwrap()),
        "GET",
        "/$/auth/device/AAAA-AAAA",
        &[("cookie", &carol)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // the token endpoint rejects nonsense
    let bad = form(&s.app, "/$/auth/token", "grant_type=password").await;
    assert_eq!(bad.json()["error"], "unsupported_grant_type");
}

#[tokio::test]
async fn loopback_flow() {
    let s = auth_server();
    let (cookie, csrf) = password_session(&s, "alice").await;
    let verifier = crate::auth::crypto::random_token(32);
    let challenge = pkce_challenge(&verifier);
    let authorize = |port: u32, state: &'static str| {
        let app = s.app.clone();
        let (cookie, csrf, challenge) = (cookie.clone(), csrf.clone(), challenge.clone());
        async move {
            call(
                &app,
                "POST",
                "/$/auth/cli/authorize",
                &session_json(&cookie, &csrf),
                &format!(
                    r#"{{"port":{port},"state":"{state}","codeChallenge":"{challenge}","name":"laptop","label":"sparkles CLI","hostname":"h","expiresIn":"7d"}}"#
                ),
            )
            .await
        }
    };
    let r = authorize(50123, "s1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let redirect = r.json()["redirect"].as_str().unwrap().to_string();
    let re = regex::Regex::new(
        r"^http://127\.0\.0\.1:50123/callback\?code=([A-Za-z0-9_-]{43})&state=s[0-9]$",
    )
    .unwrap();
    let code = re.captures(&redirect).unwrap()[1].to_string();
    assert_eq!(authorize(80, "s1").await.status, StatusCode::BAD_REQUEST);

    let exchange = |code: String, verifier: String| {
        let app = s.app.clone();
        async move {
            form(
                &app,
                "/$/auth/token",
                &format!(
                    "grant_type=authorization_code&code={code}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%3A50123%2Fcallback"
                ),
            )
            .await
        }
    };
    let wrong = exchange(code.clone(), "x".repeat(43)).await;
    assert_eq!(wrong.json()["error"], "invalid_grant");
    // the wrong verifier consumed the code
    let late = exchange(code, verifier.clone()).await;
    assert_eq!(late.json()["error"], "invalid_grant");

    let r = authorize(50123, "s2").await;
    let code = re.captures(r.json()["redirect"].as_str().unwrap()).unwrap()[1].to_string();
    let ok = exchange(code.clone(), verifier.clone()).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    let token = ok.json()["access_token"].as_str().unwrap().to_string();
    let l = get_as(&s.app, "/$/auth/tokens", Some(&bearer(&token)))
        .await
        .json();
    assert!(
        l["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["via"] == "cli-loopback")
    );
    assert_eq!(
        exchange(code, verifier.clone()).await.json()["error"],
        "invalid_grant"
    );

    // codes expire after 120 s
    let r = authorize(50123, "s3").await;
    let code = re.captures(r.json()["redirect"].as_str().unwrap()).unwrap()[1].to_string();
    s.auth().advance(121);
    assert_eq!(
        exchange(code, verifier).await.json()["error"],
        "invalid_grant"
    );
}
