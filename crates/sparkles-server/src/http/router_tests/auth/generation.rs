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
