//! Ordinary routes use the same pre-lookup generation handshake as managed work.

use super::*;

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
        let refused = pending.await.unwrap();
        assert_eq!(
            refused.status,
            StatusCode::FORBIDDEN,
            "stale authentication must stop before {uri}: {}",
            refused.text()
        );
        let current = call(&server.app, "GET", uri, &[("authorization", &b("bob"))], "").await;
        let expected = if uri == "/$/whoami" {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(current.status, expected, "{}", current.text());
    }
}
