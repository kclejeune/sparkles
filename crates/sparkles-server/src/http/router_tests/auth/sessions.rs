//! UI sessions: password and token logins, the CSRF synchronizer, logout, tampered
//! cookies, persistence and key rotation.

use super::tokens::mint_as;
use super::*;

async fn login(s: &AuthServer, body: &str, origin: &str) -> R {
    call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[("content-type", "application/json"), ("origin", origin)],
        body,
    )
    .await
}

#[tokio::test]
async fn password_login_and_session_csrf() {
    let s = auth_server();
    let r = login(
        &s,
        r#"{"user":"bob","password":"bob-pw"}"#,
        "http://localhost:3030",
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let set = r.set_cookie("sparkles_session").unwrap();
    // http://localhost: no __Host- prefix and no Secure (development)
    assert!(set.starts_with("sparkles_session="), "{set}");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax") && set.contains("Path=/"));
    assert!(
        set.contains("Max-Age=43200") && !set.contains("Secure"),
        "{set}"
    );
    let cookie = r.cookie("sparkles_session").unwrap();
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["principal"]["name"], "bob");
    assert_eq!(who["method"], "session");
    assert_eq!(who["logout"], true);
    let csrf = who["csrfToken"].as_str().unwrap().to_string();

    let upd = |extra: Vec<(&'static str, String)>| {
        let app = s.app.clone();
        let cookie = cookie.clone();
        async move {
            let mut h: Vec<(&str, &str)> = vec![
                ("cookie", &cookie),
                ("content-type", "application/sparql-update"),
            ];
            for (k, v) in &extra {
                h.push((k, v));
            }
            call(&app, "POST", "/wiki/update", &h, INSERT).await
        }
    };
    let none = upd(vec![]).await;
    assert_eq!(none.status, StatusCode::FORBIDDEN);
    assert_eq!(none.json()["error"], "CSRF token missing or invalid");
    let wrong = upd(vec![("x-sparkles-csrf", "x".repeat(43))]).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let ok = upd(vec![("x-sparkles-csrf", csrf.clone())]).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    // reads need no CSRF token
    let q = call(
        &s.app,
        "GET",
        &format!("/wiki{ASK}"),
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(q.status, StatusCode::OK);
    assert!(metric(&s.app, "sparkles_auth_denied_total{kind=\"csrf\"}").await >= 2);
    assert!(
        metric(
            &s.app,
            "sparkles_auth_logins_total{method=\"password\",result=\"ok\"}"
        )
        .await
            >= 1
    );

    // logout ends the session and clears the cookie
    let lo = call(
        &s.app,
        "POST",
        "/$/auth/logout",
        &[("cookie", &cookie), ("x-sparkles-csrf", &csrf)],
        "",
    )
    .await;
    assert_eq!(lo.status, StatusCode::OK);
    assert_eq!(lo.json()["redirect"], J::Null);
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
    // the stale cookie is cleared on the way
    assert!(
        after
            .set_cookie("sparkles_session")
            .unwrap()
            .contains("Max-Age=0")
    );
}

#[tokio::test]
async fn login_failures_and_login_csrf() {
    let s = auth_server();
    let wrong = login(
        &s,
        r#"{"user":"bob","password":"nope"}"#,
        "http://localhost:3030",
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert!(wrong.set_cookie("sparkles_session").is_none());
    let cross = login(
        &s,
        r#"{"user":"bob","password":"bob-pw"}"#,
        "https://evil.example",
    )
    .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    assert_eq!(cross.json()["error"], "cross-origin request refused");
    let bad = login(&s, r#"{"nothing":1}"#, "http://localhost:3030").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let stat = login(
        &s,
        &format!(r#"{{"token":"{}"}}"#, t_prom()),
        "http://localhost:3030",
    )
    .await;
    assert_eq!(stat.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_login_dies_with_its_token() {
    let s = auth_server();
    let m = mint_as(
        &s.app,
        &[("authorization", &b("bob"))],
        r#"{"name":"ui","datasets":{"wiki":"read"},"expiresIn":"1h"}"#,
    )
    .await;
    let token = m.json()["token"].as_str().unwrap().to_string();
    let id = m.json()["id"].as_str().unwrap().to_string();
    let r = login(
        &s,
        &format!(r#"{{"token":"{token}"}}"#),
        "http://localhost:3030",
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // the session ends no later than the token (a second may pass between the two)
    let set = r.set_cookie("sparkles_session").unwrap();
    let max_age: u64 = set
        .split(';')
        .find_map(|a| a.trim().strip_prefix("Max-Age="))
        .and_then(|v| v.parse().ok())
        .unwrap();
    assert!((3590..=3600).contains(&max_age), "{set}");
    let cookie = r.cookie("sparkles_session").unwrap();
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["principal"]["kind"], "token");
    assert_eq!(who["principal"]["name"], id.as_str());
    assert_eq!(who["datasets"], serde_json::json!({ "wiki": "read" }));
    let del = call(
        &s.app,
        "DELETE",
        &format!("/$/auth/tokens/{id}"),
        &[("authorization", &b("bob"))],
        "",
    )
    .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["principal"]["kind"], "anonymous");
}

#[tokio::test]
async fn tampered_cookies_are_anonymous() {
    let s = auth_server();
    let (cookie, _) = password_session(&s, "bob").await;
    let (name, value) = cookie.split_once('=').unwrap();
    let (id, _sig) = value.rsplit_once('.').unwrap();
    let forged = format!("{name}={id}.{}", "A".repeat(43));
    let r = call(&s.app, "GET", "/$/whoami", &[("cookie", &forged)], "").await;
    assert_eq!(r.json()["principal"]["kind"], "anonymous");
    assert!(
        r.set_cookie("sparkles_session")
            .unwrap()
            .contains("Max-Age=0")
    );
    // public data stays readable with a stale cookie
    let p = call(
        &s.app,
        "GET",
        &format!("/public{ASK}"),
        &[("cookie", &forged)],
        "",
    )
    .await;
    assert_eq!(p.status, StatusCode::OK);
}

#[tokio::test]
async fn sessions_survive_restarts_until_the_key_changes() {
    let s = auth_server();
    let (cookie, _) = password_session(&s, "bob").await;
    let raw = cookie
        .split_once('=')
        .unwrap()
        .1
        .rsplit_once('.')
        .unwrap()
        .0
        .to_string();
    let file = std::fs::read_to_string(s.dir.path().join("auth/sessions.json")).unwrap();
    assert!(!file.contains(&raw));
    assert_eq!(
        serde_json::from_str::<J>(&file).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let s = s.restart();
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["principal"]["name"], "bob");
    std::fs::remove_file(s.dir.path().join("auth/session.key")).unwrap();
    let s = s.restart();
    let who = call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    assert_eq!(who["principal"]["kind"], "anonymous");
}

#[tokio::test]
async fn sessions_follow_the_policy() {
    let s = auth_server();
    let (cookie, _) = password_session(&s, "bob").await;
    let r = call(
        &s.app,
        "GET",
        &format!("/wiki{ASK}"),
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    s.write_config(&Fixture {
        bob_wiki: false,
        ..Default::default()
    });
    s.auth().reload().unwrap();
    let r = call(
        &s.app,
        "GET",
        &format!("/wiki{ASK}"),
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_owner_cannot_log_everyone_out() {
    use crate::auth::MAX_SESSIONS_PER_OWNER;
    let s = auth_server();
    let (alice, _) = password_session(&s, "alice").await;
    // bob mints one token and signs in with it over and over
    let m = mint_as(&s.app, &[("authorization", &b("bob"))], r#"{"name":"ui"}"#).await;
    let token = m.json()["token"].as_str().unwrap().to_string();
    let body = format!(r#"{{"token":"{token}"}}"#);
    let mut cookies = Vec::new();
    for _ in 0..MAX_SESSIONS_PER_OWNER + 5 {
        let r = login(&s, &body, "http://localhost:3030").await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
        cookies.push(r.cookie("sparkles_session").unwrap());
    }
    let kind = |cookie: String| {
        let app = s.app.clone();
        async move {
            let who = call(&app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
                .await
                .json();
            who["principal"]["kind"].as_str().unwrap().to_string()
        }
    };
    // his own oldest sessions ended, alice's did not
    assert_eq!(kind(cookies[0].clone()).await, "anonymous");
    assert_eq!(kind(cookies[4].clone()).await, "anonymous");
    assert_eq!(kind(cookies[5].clone()).await, "token");
    assert_eq!(kind(cookies.last().unwrap().clone()).await, "token");
    assert_eq!(kind(alice).await, "user");
    let now = s.auth().now();
    assert_eq!(s.auth().sessions.active(now), MAX_SESSIONS_PER_OWNER + 1);
}
