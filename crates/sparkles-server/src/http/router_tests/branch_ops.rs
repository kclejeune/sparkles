//! Branches and merges over HTTP, Phase 2 of F09: squash merges, reverts,
//! cherry-picks, replayed fast-forwards, renames, deletions that re-parent, exempt
//! predicates and merges as tasks (the acceptance examples A24 onward).

use super::branches::{get, json_req, setup, subjects, update};
use super::*;
use serde_json::json;

#[tokio::test]
async fn a24_squash_merges() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(&app, "ds@dev", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    let (r, _) = get(&app, "/$/merge/ds?source=dev&squash=true").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["squashed"], true);
    let (r, h) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "squash": true }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["merged"], true);
    assert_eq!(m["squashed"], true);
    assert_eq!(m["commit"]["kind"], "merge");
    assert_eq!(m["commit"]["message"], "squash dev (commit 4) into main");
    assert!(m["commit"].get("mergedFrom").is_none(), "{m}");
    assert_eq!(commit_header(&h), 3);
    assert_eq!(
        subjects(&app, "/ds/sparql").await,
        subjects(&app, "/ds@dev/sparql").await
    );
    // no second parent: dev is still ahead, and the listing shows no mergedFrom
    let (r, _) = get(&app, "/$/branches/ds/dev").await;
    assert_eq!(r.json()["ahead"], 2);
    let (r, _) = get(&app, "/$/commits/ds?limit=1").await;
    assert!(r.json()["commits"][0].get("mergedFrom").is_none());
    // nothing new: no commit
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "squash": true }),
    )
    .await;
    assert_eq!(r.json()["upToDate"], true, "{}", r.text());
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "squash": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

async fn post(app: &Router, path: &str, body: &str) -> (Resp, axum::http::HeaderMap) {
    send_h(
        app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn a25_reverts() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    update(&app, "ds", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(
        &app,
        "ds",
        "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 31 }",
    )
    .await;
    let (r, _) = get(&app, "/$/revert/ds?commit=3").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["changes"]["deleted"], 1);
    assert_eq!(r.json()["merged"], false);
    let (r, h) = post(&app, "/$/revert/ds?commit=3", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["commit"]["kind"], "revert");
    assert_eq!(m["commit"]["seq"], 5);
    assert_eq!(m["commit"]["message"], "revert commit 3");
    assert!(m["commit"].get("mergedFrom").is_none(), "{m}");
    assert_eq!(m["reverted"], json!({ "branch": "main", "seq": 3 }));
    assert_eq!(m["base"]["seq"], 3);
    assert_eq!(m["source"]["seq"], 2);
    assert_eq!(commit_header(&h), 5);
    assert_eq!(subjects(&app, "/ds/sparql").await, ["urn:a", "urn:b"]);
    let (r, _) = post(&app, "/$/revert/ds?commit=3", "{}").await;
    assert_eq!(r.json()["upToDate"], true, "{}", r.text());
    // a later change to the same cell conflicts
    let (r, _) = post(&app, "/$/revert/ds?commit=1", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "merge-conflict");
    assert_eq!(r.json()["cells"][0]["predicate"], "<urn:age>");
    let (r, _) = post(
        &app,
        "/$/revert/ds?commit=1",
        r#"{"onConflict":"theirs","message":"no age"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["commit"]["message"], "no age");
    assert_eq!(subjects(&app, "/ds/sparql").await, ["urn:b"]);
    // on a branch, of a commit it shares with main
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    let (r, _) = post(&app, "/$/revert/ds?branch=dev&commit=2", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["target"]["branch"], "dev");
    assert!(subjects(&app, "/ds@dev/sparql").await.is_empty());
    assert_eq!(subjects(&app, "/ds/sparql").await, ["urn:b"]);
    // errors
    for (path, body, status) in [
        ("/$/revert/ds?commit=0", "", StatusCode::BAD_REQUEST),
        ("/$/revert/ds", "", StatusCode::BAD_REQUEST),
        ("/$/revert/ds?commit=99", "", StatusCode::NOT_FOUND),
        (
            "/$/revert/ds?commit=2&branch=nope",
            "",
            StatusCode::NOT_FOUND,
        ),
        (
            "/$/revert/ds?commit=2",
            r#"{"squash":true}"#,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (r, _) = post(&app, path, body).await;
        assert_eq!(r.status, status, "{path} {body}: {}", r.text());
    }
    json_req(
        &app,
        "PATCH",
        "/$/branches/ds/main",
        json!({ "protected": true }),
    )
    .await;
    let (r, _) = post(&app, "/$/revert/ds?commit=2", "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["code"], "branch-protected");
}
