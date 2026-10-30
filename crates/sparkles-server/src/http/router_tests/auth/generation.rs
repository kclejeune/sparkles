//! Ordinary routes use the same pre-lookup generation handshake as managed work.

use super::*;

/// A Basic authentication whose verification overlaps a reload is decided again
/// under the new policy: the old grants never reach the route, and the caller gets
/// the current answer instead of a refusal.
#[tokio::test]
async fn old_async_password_lookup_cannot_enter_an_ordinary_route_after_reload() {
    for uri in [
        "/wiki/query?query=SELECT%20(1%20AS%20%3Fx)%20WHERE%20%7B%7D",
        "/$/whoami",
    ] {
        let server = build(Fixture::default());
        let auth = server.auth();
        let hold = auth.hold_verifications();
        let app = server.app.clone();
        let pending = tokio::spawn(async move {
            call(&app, "GET", uri, &[("authorization", &b("bob"))], "").await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while auth.argon_load().1 == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ordinary request must be queued before policy replacement");
        server.write_config(&Fixture {
            bob_wiki: false,
            ..server.fixture.clone()
        });
        auth.reload().unwrap();
        drop(hold);
        let recomputed = pending.await.unwrap();
        let current = call(&server.app, "GET", uri, &[("authorization", &b("bob"))], "").await;
        let expected = if uri == "/$/whoami" {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(current.status, expected, "{}", current.text());
        assert_eq!(
            recomputed.status,
            expected,
            "stale authentication must not reach {uri}: {}",
            recomputed.text()
        );
        if uri == "/$/whoami" {
            assert_eq!(recomputed.json()["datasets"], current.json()["datasets"]);
        }
    }
}

/// An open event stream of `uri` with the request headers `h`.
async fn open_stream(s: &AuthServer, uri: &str, h: &[(&str, &str)]) -> axum::body::BodyDataStream {
    let mut req = Request::get(uri)
        .header("accept", "text/event-stream")
        .extension(ConnectInfo(default_peer()));
    for (k, v) in h {
        req = req.header(*k, *v);
    }
    let res = s
        .app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{uri}");
    res.into_body().into_data_stream()
}

/// The next named event of an event stream: its type and data.
async fn next_event(body: &mut axum::body::BodyDataStream, buf: &mut String) -> (String, String) {
    use futures_util::StreamExt;
    loop {
        if let Some(end) = buf.find("\n\n") {
            let ev: String = buf.drain(..end + 2).collect();
            let field = |k: &str| {
                ev.lines()
                    .filter_map(|l| l.strip_prefix(k))
                    .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if !field("event:").is_empty() {
                return (field("event:"), field("data:"));
            }
            continue;
        }
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("an event within 10 s")
            .expect("the stream is open")
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
}

async fn logout(s: &AuthServer, cookie: &str, csrf: &str) {
    let r = call(
        &s.app,
        "POST",
        "/$/auth/logout",
        &[("cookie", cookie), ("x-sparkles-csrf", csrf)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

fn insert(n: u32) -> String {
    format!("INSERT DATA {{ <urn:x:s{n}> <urn:x:p> {n} }}")
}

/// Signing in and out issues and revokes only that user's credentials: another
/// user's change feed and live query keep streaming, and ending the stream owner's
/// own session ends them with `access-changed`.
#[tokio::test]
async fn streams_end_only_when_their_own_credential_is_revoked() {
    let s = build(Fixture::default());
    let (bob, bob_csrf) = password_session(&s, "bob").await;
    let after = head(&s.state, "wiki");
    let feed_uri = format!("/wiki/changes?after={after}");
    let mut feed = open_stream(&s, &feed_uri, &[("cookie", &bob)]).await;
    let mut feed_buf = String::new();
    let live_uri = format!(
        "/wiki/live?query={}",
        "SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Curn%3Ax%3Ap%3E%20%3Fo%20%7D"
    );
    let mut live = open_stream(&s, &live_uri, &[("cookie", &bob)]).await;
    let mut live_buf = String::new();
    assert_eq!(next_event(&mut live, &mut live_buf).await.0, "snapshot");
    // a stream of another credential, kept open across bob's logout
    let carol = b("carol");
    let mut other = open_stream(&s, &feed_uri, &[("authorization", &carol)]).await;
    let mut other_buf = String::new();

    // alice signs in and out: no right of bob's changes
    let (alice, alice_csrf) = password_session(&s, "alice").await;
    logout(&s, &alice, &alice_csrf).await;
    let r = update_as(&s.app, "wiki", &b("alice"), &insert(1)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (event, data) = next_event(&mut feed, &mut feed_buf).await;
    assert_eq!(
        event, "commit",
        "another user's logout ended the feed: {data}"
    );
    let (event, data) = next_event(&mut live, &mut live_buf).await;
    assert_eq!(
        event, "diff",
        "another user's logout ended the live query: {data}"
    );
    assert_eq!(next_event(&mut other, &mut other_buf).await.0, "commit");

    // bob's own logout ends both of his streams, and not carol's
    logout(&s, &bob, &bob_csrf).await;
    let r = update_as(&s.app, "wiki", &b("alice"), &insert(2)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (event, data) = next_event(&mut feed, &mut feed_buf).await;
    assert_eq!(event, "error", "{data}");
    assert!(data.contains("access-changed"), "{data}");
    let (event, data) = next_event(&mut live, &mut live_buf).await;
    assert_eq!(event, "error", "{data}");
    assert!(data.contains("access-changed"), "{data}");
    let (event, data) = next_event(&mut other, &mut other_buf).await;
    assert_eq!(event, "commit", "bob's logout ended carol's feed: {data}");
}

/// A Basic credential verified once stays cached while other users sign in and out.
#[tokio::test]
async fn another_users_login_does_not_reverify_cached_basic_credentials() {
    let s = build(Fixture::default());
    let ask = format!("/wiki{ASK}");
    let verified = || metric(&s.app, "sparkles_auth_password_verifications_total");
    assert_eq!(
        get_as(&s.app, &ask, Some(&b("bob"))).await.status,
        StatusCode::OK
    );
    let before = verified().await;
    let (alice, alice_csrf) = password_session(&s, "alice").await;
    assert_eq!(
        get_as(&s.app, &ask, Some(&b("bob"))).await.status,
        StatusCode::OK
    );
    logout(&s, &alice, &alice_csrf).await;
    assert_eq!(
        get_as(&s.app, &ask, Some(&b("bob"))).await.status,
        StatusCode::OK
    );
    assert_eq!(
        verified().await,
        before + 1,
        "only alice's login may verify a password"
    );
}

/// Run `revoke` once, right after the next stored credential record is copied by
/// an authentication and before that authentication is accepted.
fn revoke_after_lookup(s: &AuthServer, revoke: impl FnOnce(&crate::auth::Auth) + Send + 'static) {
    let mut revoke = Some(revoke);
    s.auth().hooks.lock().after_lookup = Some(Box::new(move |auth| {
        if let Some(r) = revoke.take() {
            r(auth);
        }
    }));
}

/// Mint a token for bob that reads wiki; its secret and id.
async fn bob_token(s: &AuthServer) -> (String, String) {
    let r = super::tokens::mint_as(
        &s.app,
        &[("authorization", &b("bob"))],
        r#"{"name":"race","datasets":{"wiki":"read"}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    (
        j["token"].as_str().unwrap().to_string(),
        j["id"].as_str().unwrap().to_string(),
    )
}

/// A token or session revoked between its lookup and the acceptance of the
/// authentication is refused, as a credential that was already revoked would be.
#[tokio::test]
async fn revocation_during_lookup_refuses_the_request() {
    let s = build(Fixture::default());
    let ask = format!("/wiki{ASK}");
    for basic_password in [false, true] {
        let (token, id) = bob_token(&s).await;
        let header = if basic_password {
            basic("bob", &token)
        } else {
            bearer(&token)
        };
        assert_eq!(
            get_as(&s.app, &ask, Some(&header)).await.status,
            StatusCode::OK
        );
        revoke_after_lookup(&s, move |auth| {
            assert_eq!(auth.tokens.remove(&[id]).unwrap(), 1);
        });
        let r = get_as(&s.app, &ask, Some(&header)).await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "a token revoked during its lookup was accepted: {}",
            r.text()
        );
        s.auth().hooks.lock().after_lookup = None;
    }
    let (cookie, _) = password_session(&s, "bob").await;
    let get = |cookie: String| {
        let app = s.app.clone();
        let ask = ask.clone();
        async move { call(&app, "GET", &ask, &[("cookie", &cookie)], "").await }
    };
    assert_eq!(get(cookie.clone()).await.status, StatusCode::OK);
    // every session ends, as an expiry prune at the end of time does
    revoke_after_lookup(&s, |auth| auth.sessions.prune(i64::MAX, None).unwrap());
    let r = get(cookie).await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "a session revoked during its lookup was accepted: {}",
        r.text()
    );
}

/// A token login whose token is revoked after its session was created, before the
/// login answers, removes that session and refuses the login.
#[tokio::test]
async fn token_login_revoked_after_issuance_opens_no_session() {
    let s = build(Fixture {
        extra: "[session]\ntoken_login = true\n".into(),
        ..Default::default()
    });
    let (token, id) = bob_token(&s).await;
    let mut revoke = Some(id);
    s.auth().hooks.lock().after_issue = Some(Box::new(move |auth| {
        if let Some(id) = revoke.take() {
            assert_eq!(auth.sessions.active(auth.now()), 1, "the session exists");
            assert_eq!(auth.tokens.remove(&[id]).unwrap(), 1);
        }
    }));
    let r = call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[
            ("content-type", "application/json"),
            ("origin", s.fixture.public_url),
        ],
        &format!(r#"{{"token":"{token}"}}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
    assert!(r.cookie("sparkles_session").is_none());
    assert_eq!(s.auth().sessions.active(s.auth().now()), 0);
}

/// A login whose verification overlaps a reload gets the session lifetime of the
/// policy it was finally verified under.
#[tokio::test]
async fn login_across_a_reload_uses_the_new_session_lifetime() {
    let s = build(Fixture::default());
    let auth = s.auth();
    let hold = auth.hold_verifications();
    let app = s.app.clone();
    let origin = s.fixture.public_url;
    let pending = tokio::spawn(async move {
        call(
            &app,
            "POST",
            "/$/auth/login",
            &[("content-type", "application/json"), ("origin", origin)],
            r#"{"user":"bob","password":"bob-pw"}"#,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while auth.argon_load().1 == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the login must be verifying before the reload");
    s.write_config(&Fixture {
        extra: "[session]\nttl = \"1h\"\n".into(),
        ..s.fixture.clone()
    });
    auth.reload().unwrap();
    drop(hold);
    let r = pending.await.unwrap();
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let set = r.set_cookie("sparkles_session").unwrap();
    assert!(set.contains("Max-Age=3600"), "{set}");
}

/// Authority changes that keep overlapping a request cost at most one password
/// verification for each of its attempts.
#[tokio::test]
async fn overlapping_changes_bound_the_verifications_of_one_request() {
    let s = build(Fixture::default());
    let auth = s.auth();
    auth.hooks.lock().after_verify = Some(Box::new(|auth| {
        drop(auth.hold_authority_update());
    }));
    let verified = || metric(&s.app, "sparkles_auth_password_verifications_total");
    let before = verified().await;
    let r = call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[
            ("content-type", "application/json"),
            ("origin", s.fixture.public_url),
        ],
        r#"{"user":"bob","password":"bob-pw"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.err()["code"], "authority-changed");
    let login = verified().await - before;
    let r = get_as(&s.app, &format!("/wiki{ASK}"), Some(&b("bob"))).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    let basic = verified().await - before - login;
    auth.hooks.lock().after_verify = None;
    assert_eq!(
        (login, basic),
        (
            u64::from(crate::auth::AUTH_ATTEMPTS),
            u64::from(crate::auth::AUTH_ATTEMPTS)
        )
    );
}

/// Basic credentials on a password login share the request's verification budget
/// with the login itself.
#[tokio::test]
async fn basic_headers_and_a_password_login_share_one_verification_budget() {
    let s = build(Fixture::default());
    let auth = s.auth();
    let verified = || metric(&s.app, "sparkles_auth_password_verifications_total");
    let before = verified().await;
    // Every verification overlaps an update except the third. The Basic header is
    // then accepted on the middleware's last attempt, and each verification of the
    // login body overlaps one.
    let mut n = 0;
    auth.hooks.lock().after_verify = Some(Box::new(move |auth| {
        n += 1;
        if n != 3 {
            drop(auth.hold_authority_update());
        }
    }));
    let r = call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[
            ("content-type", "application/json"),
            ("origin", s.fixture.public_url),
            ("authorization", &b("alice")),
        ],
        r#"{"user":"bob","password":"bob-pw"}"#,
    )
    .await;
    auth.hooks.lock().after_verify = None;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.err()["code"], "authority-changed");
    assert_eq!(
        verified().await - before,
        u64::from(crate::auth::AUTH_ATTEMPTS),
        "one request ran more password verifications than its attempts"
    );
    // without overlapping changes the same request signs in
    let r = call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[
            ("content-type", "application/json"),
            ("origin", s.fixture.public_url),
            ("authorization", &b("alice")),
        ],
        r#"{"user":"bob","password":"bob-pw"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
}

/// Open a change feed and a live query on wiki with the request headers `h`.
async fn open_both(
    s: &AuthServer,
    h: &[(&str, &str)],
) -> (
    (axum::body::BodyDataStream, String),
    (axum::body::BodyDataStream, String),
) {
    let after = head(&s.state, "wiki");
    let feed = open_stream(s, &format!("/wiki/changes?after={after}"), h).await;
    let live_uri = format!(
        "/wiki/live?query={}",
        "SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Curn%3Ax%3Ap%3E%20%3Fo%20%7D"
    );
    let mut live = open_stream(s, &live_uri, h).await;
    let mut live_buf = String::new();
    assert_eq!(next_event(&mut live, &mut live_buf).await.0, "snapshot");
    ((feed, String::new()), (live, live_buf))
}

/// After the credential's expiry, the next commit ends both streams with
/// `access-changed` instead of sending it.
async fn expect_ended(
    s: &AuthServer,
    streams: &mut (
        (axum::body::BodyDataStream, String),
        (axum::body::BodyDataStream, String),
    ),
    n: u32,
) {
    let r = update_as(&s.app, "wiki", &b("alice"), &insert(n)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for (body, buf) in [&mut streams.0, &mut streams.1] {
        let (event, data) = next_event(body, buf).await;
        assert_eq!(
            event, "error",
            "an expired credential kept streaming: {data}"
        );
        assert!(data.contains("access-changed"), "{data}");
    }
}

/// Streams opened with a session end when the session reaches its absolute
/// lifetime or its idle timeout, and streams of a token end when it expires.
#[tokio::test]
async fn streams_end_when_their_credential_expires() {
    // the absolute lifetime of a session: twelve hours by default
    let s = build(Fixture::default());
    let (cookie, _) = password_session(&s, "bob").await;
    let mut streams = open_both(&s, &[("cookie", &cookie)]).await;
    s.auth().advance(43_200);
    expect_ended(&s, &mut streams, 1).await;

    // the idle timeout of a session that a stream does not renew
    let s = build(Fixture {
        extra: "[session]\nidle_timeout = \"10m\"\n".into(),
        ..Default::default()
    });
    let (cookie, _) = password_session(&s, "bob").await;
    let mut streams = open_both(&s, &[("cookie", &cookie)]).await;
    s.auth().advance(600);
    expect_ended(&s, &mut streams, 2).await;

    // a minted token
    let s = build(Fixture::default());
    let r = super::tokens::mint_as(
        &s.app,
        &[("authorization", &b("bob"))],
        r#"{"name":"short","datasets":{"wiki":"read"},"expiresIn":"1d"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let token = bearer(r.json()["token"].as_str().unwrap());
    let mut streams = open_both(&s, &[("authorization", &token)]).await;
    s.auth().advance(86_399);
    // one second before its expiry the token still streams
    let r = update_as(&s.app, "wiki", &b("alice"), &insert(3)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        next_event(&mut streams.0.0, &mut streams.0.1).await.0,
        "commit"
    );
    assert_eq!(
        next_event(&mut streams.1.0, &mut streams.1.1).await.0,
        "diff"
    );
    s.auth().advance(1);
    expect_ended(&s, &mut streams, 4).await;
}

/// A hook that moves the auth clock `secs` forward the first time it runs, and
/// signals `ran`.
fn advance_once(secs: i64, ran: Arc<tokio::sync::Notify>) -> crate::auth::TestHook {
    let mut once = true;
    Box::new(move |auth| {
        if std::mem::take(&mut once) {
            auth.advance(secs);
            ran.notify_one();
        }
    })
}

/// A live query that is coalescing diffs when its session expires sends no merged
/// diff.
#[tokio::test]
async fn live_coalescing_sends_nothing_merged_after_expiry() {
    let s = build(Fixture::default());
    let (cookie, _) = password_session(&s, "bob").await;
    let q = "SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Curn%3Ax%3Ap%3E%20%3Fo%20%7D";
    let mut live = open_stream(
        &s,
        &format!("/wiki/live?query={q}&coalesceMs=60000"),
        &[("cookie", &cookie)],
    )
    .await;
    let mut buf = String::new();
    assert_eq!(next_event(&mut live, &mut buf).await.0, "snapshot");
    // the session expires while the first diff waits to be merged with more
    let ran = Arc::new(tokio::sync::Notify::new());
    s.auth().hooks.lock().coalescing = Some(advance_once(43_200, ran.clone()));
    let r = update_as(&s.app, "wiki", &b("alice"), &insert(1)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (event, data) = tokio::join!(next_event(&mut live, &mut buf), async {
        ran.notified().await;
        let r = update_as(&s.app, "wiki", &b("alice"), &insert(2)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    })
    .0;
    assert_eq!(event, "error", "a diff was merged after expiry: {data}");
    assert!(data.contains("access-changed"), "{data}");
}

/// The rest of a paged live-query snapshot is not sent after its session expires.
#[tokio::test]
async fn live_snapshot_pages_stop_at_expiry() {
    let q = "SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Curn%3Ax%3Ap%3E%20%3Fo%20%7D";
    let s = build(Fixture::default());
    let rows: String = (0..10_001)
        .map(|i| format!("<urn:x:r{i}> <urn:x:p> \"{i}\" .\n"))
        .collect();
    s.state
        .get("wiki")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            rows.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let (cookie, _) = password_session(&s, "bob").await;
    let mut live = open_stream(&s, &format!("/wiki/live?query={q}"), &[("cookie", &cookie)]).await;
    let mut buf = String::new();
    let (event, data) = next_event(&mut live, &mut buf).await;
    assert_eq!(event, "snapshot");
    assert!(
        data.contains("\"more\":true"),
        "the fixture needs two pages"
    );
    s.auth().advance(43_200);
    let (event, data) = next_event(&mut live, &mut buf).await;
    assert_eq!(event, "error", "a snapshot page was sent after expiry");
    assert!(data.contains("access-changed"), "{data}");
}
