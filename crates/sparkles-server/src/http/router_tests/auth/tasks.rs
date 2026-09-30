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
