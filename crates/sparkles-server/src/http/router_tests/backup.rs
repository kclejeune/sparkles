//! Backup repository routes (`/$/repositories`, `/$/backups/{ds}`,
//! `/$/backup-policies`).

use super::*;

#[tokio::test]
async fn backup_routes_are_registered() {
    let s = server();
    for (method, path) in [
        ("GET", "/$/repositories"),
        ("POST", "/$/repositories"),
        ("GET", "/$/repositories/local"),
        ("PUT", "/$/repositories/local"),
        ("DELETE", "/$/repositories/local"),
        ("POST", "/$/repositories/local/test"),
        ("POST", "/$/repositories/local/verify"),
        ("GET", "/$/repositories/local/backups"),
        ("POST", "/$/repositories/local/gc"),
        ("GET", "/$/repositories/local/locks"),
        ("DELETE", "/$/repositories/local/locks/x"),
        ("GET", "/$/backups/ds"),
        ("POST", "/$/backups/ds"),
        ("GET", "/$/backups/ds/local/b1"),
        ("DELETE", "/$/backups/ds/local/b1"),
        ("POST", "/$/backups/ds/local/b1/restore"),
        ("POST", "/$/backups/ds/local/b1/verify"),
        ("GET", "/$/backup-policies"),
        ("POST", "/$/backup-policies"),
        ("POST", "/$/backup-policies/preview"),
        ("GET", "/$/backup-policies/nightly"),
        ("PUT", "/$/backup-policies/nightly"),
        ("DELETE", "/$/backup-policies/nightly"),
        ("POST", "/$/backup-policies/nightly/run"),
        ("POST", "/$/backup-policies/nightly/retention"),
        ("GET", "/$/backup-policies/nightly/runs"),
    ] {
        // a JSON body: `POST /$/backups/{ds}` without one is Fuseki's N-Quads dump
        let r = send(
            &s.app,
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED, "{method} {path}");
        let j = r.json();
        assert_eq!(j["code"], "not-implemented", "{method} {path}: {j}");
        assert!(j["error"].is_string() && j["requestId"].is_string(), "{j}");
    }
}
