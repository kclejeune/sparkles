//! Who may see and cancel tasks: dataset tasks by the dataset's levels, server-scoped
//! tasks (no dataset) by `server-admin` only.

use super::*;
use crate::http::router_tests::tasks::{spin, wait_done};
use std::sync::atomic::AtomicBool;

fn ids(r: &R) -> Vec<String> {
    r.json()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

async fn delete_as(app: &Router, uri: &str, auth: &str) -> R {
    call(app, "DELETE", uri, &[("authorization", auth)], "").await
}

#[tokio::test]
async fn cancel_needs_admin_on_the_task_dataset() {
    let s = auth_server();
    let id = spin(&s.state, "wiki", true, Arc::default());
    let uri = format!("/$/tasks/{id}");
    // bob writes wiki: he sees the task but may not cancel it
    assert_eq!(
        get_as(&s.app, &uri, Some(&b("bob"))).await.status,
        StatusCode::OK
    );
    let r = delete_as(&s.app, &uri, &b("bob")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.err()["error"], "admin access to /wiki required");
    // anonymous cannot see it at all
    let r = call(&s.app, "DELETE", &uri, &[], "").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // carol administers wiki
    let r = delete_as(&s.app, &uri, &b("carol")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(wait_done(&s.state, &id).await.state, "cancelled");
}

/// The route table's needs for the backup routes (their handlers add their own checks).
#[cfg(feature = "backup")]
#[tokio::test]
async fn backup_routes_are_authorized() {
    let s = auth_server();
    let status = |m: &'static str, uri: &'static str, user: &'static str| {
        let app = s.app.clone();
        async move {
            call(&app, m, uri, &[("authorization", &b(user))], "")
                .await
                .status
        }
    };
    let passed = StatusCode::NOT_IMPLEMENTED;
    // the repository listing is open (and filtered by its handler)
    assert_eq!(status("GET", "/$/repositories", "bob").await, passed);
    for (m, uri) in [
        ("POST", "/$/repositories"),
        ("GET", "/$/repositories/local"),
        ("GET", "/$/repositories/local/backups"),
        ("POST", "/$/backup-policies/preview"),
        ("GET", "/$/backup-policies"),
    ] {
        assert_eq!(
            status(m, uri, "carol").await,
            StatusCode::FORBIDDEN,
            "{m} {uri}"
        );
        assert_eq!(status(m, uri, "alice").await, passed, "{m} {uri}");
    }
    // per-dataset routes: read to list and show, admin for the rest
    assert_eq!(status("GET", "/$/backups/wiki", "bob").await, passed);
    assert_eq!(
        status("POST", "/$/backups/wiki", "bob").await,
        StatusCode::FORBIDDEN
    );
    // a JSON body is the repository API; without one it is Fuseki's N-Quads dump
    let json = call(
        &s.app,
        "POST",
        "/$/backups/wiki",
        &[
            ("authorization", &b("carol")),
            ("content-type", "application/json"),
        ],
        "{}",
    )
    .await;
    assert_eq!(json.status, passed);
    assert_eq!(
        status("POST", "/$/backups/wiki", "carol").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        status("POST", "/$/backups/wiki/local/b1/restore", "bob").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status("DELETE", "/$/backups/wiki/local/b1", "carol").await,
        passed
    );
    // a dataset carol cannot see stays hidden
    assert_eq!(
        status("GET", "/$/backups/secret/local/b1", "carol").await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn server_scoped_tasks_are_for_server_admins() {
    let s = auth_server();
    let stop = Arc::new(AtomicBool::new(false));
    let id = spin(&s.state, "", true, stop.clone());
    let uri = format!("/$/tasks/{id}");
    for user in ["bob", "carol"] {
        let r = get_as(&s.app, "/$/tasks", Some(&b(user))).await;
        assert!(!ids(&r).contains(&id), "{user} sees {id}");
        assert_eq!(
            get_as(&s.app, &uri, Some(&b(user))).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            delete_as(&s.app, &uri, &b(user)).await.status,
            StatusCode::NOT_FOUND
        );
    }
    let r = get_as(&s.app, "/$/tasks", Some(&b("alice"))).await;
    assert!(ids(&r).contains(&id));
    let r = delete_as(&s.app, &uri, &b("alice")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(wait_done(&s.state, &id).await.state, "cancelled");
}
