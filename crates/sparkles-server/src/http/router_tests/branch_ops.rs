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
